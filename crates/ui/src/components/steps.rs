use crate::{component, Attrs, Slot, UiEnum, UiRecord};

/// Where a step stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum WizardState {
    Done,
    Current,
    Next,
}

/// One segment of a wizard's strip.
#[derive(Clone, Debug, PartialEq, Eq, UiRecord)]
pub struct WizardStep {
    /// The stage ("Wallet").
    pub label: String,
    /// Where it stands, under the label: "Complete", "Next", or the task
    /// in hand and its position ("Back up · step 2 of 3").
    pub detail: String,
    /// The stage's tasks done: its bar fills by these.
    pub done: usize,
    /// The stage's tasks; 1 for a stage with none of its own.
    pub tasks: usize,
    pub state: WizardState,
}

component! {
    /// Progress through a flow (setup: Store, Wallet, Done), as flat
    /// segments, each with a bar counting its tasks done. It shows where
    /// you are; it isn't navigation.
    pub struct WizardSteps {
        /// What the steps are of ("Setup progress").
        label: String,
        steps: Vec<WizardStep>,
    }
    html {
        ol class="mk-wizard-steps" data-design="strip" aria-label=(label) {
            @for step in steps {
                li class="done" if step.state == WizardState::Done aria-current="step" if step.state == WizardState::Current {
                    span { (step.label) small { (step.detail) } }
                    progress class="mk-wizard-progress" value=(step.done) max=(step.tasks)
                        aria-label={ (step.label) ": " (step.done) " of " (step.tasks) " tasks complete" } {}
                }
            }
        }
    }
}

pub(super) fn wizard_steps_examples() -> Vec<(&'static str, WizardSteps)> {
    let step = |label: &str, detail: &str, done, tasks, state| WizardStep {
        label: label.into(),
        detail: detail.into(),
        done,
        tasks,
        state,
    };
    let setup = |wallet: WizardStep, done: WizardStep| {
        let store = step("Store", "Complete", 1, 1, WizardState::Done);
        WizardSteps::new("Setup progress", vec![store, wallet, done])
    };
    let next = || step("Done", "Next", 0, 1, WizardState::Next);
    vec![
        (
            "Setup, backing up a new wallet",
            setup(
                step("Wallet", "Back up · step 2 of 3", 1, 3, WizardState::Current),
                next(),
            ),
        ),
        (
            "Setup, bringing your own wallet",
            setup(
                step("Wallet", "Keys · step 2 of 2", 1, 2, WizardState::Current),
                next(),
            ),
        ),
        (
            "Setup, checking the backup",
            setup(
                step("Wallet", "Check · step 3 of 3", 2, 3, WizardState::Current),
                next(),
            ),
        ),
        (
            "Setup, finished",
            setup(
                step("Wallet", "Complete", 1, 1, WizardState::Done),
                step("Done", "Complete", 1, 1, WizardState::Current),
            ),
        ),
        (
            "Adding a wallet on its own",
            WizardSteps::new(
                "Wallet progress",
                vec![step(
                    "Wallet",
                    "Keys · step 2 of 2",
                    1,
                    2,
                    WizardState::Current,
                )],
            ),
        ),
    ]
}

/// How far a step of [`ProgressSteps`] has got.
#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum StepProgress {
    Done,
    /// Where things are now.
    Now,
    /// Begun, not finished (part of a payment in).
    Part,
    /// Stopped here (expired, reversed).
    Fail,
    /// Still to come.
    Todo,
}

/// One step of [`ProgressSteps`].
#[derive(Clone, Debug, PartialEq, Eq, UiRecord)]
pub struct ProgressStep {
    pub label: String,
    pub progress: StepProgress,
}

component! {
    /// Where something is in a short, fixed sequence (a payment: Send,
    /// Confirm, Paid), with a line saying what that means and what to do:
    /// the same place and shape in every state, so nothing moves as it
    /// changes. The colours follow `state`; the words say it all.
    pub struct ProgressSteps {
        /// What the steps are of ("Payment progress").
        label: String,
        steps: Vec<ProgressStep>,
        /// The headline of the line under the steps ("Waiting for payment").
        title: String,
        /// The rest of the line.
        message: Slot,
        /// The colours: an order's state.
        state: Option<crate::OrderState>,
        /// The POS's size: smaller type, the same shape.
        #[default(false)]
        compact: bool,
        rest: Attrs,
    }
    html {
        div class=join["mk-stage", { "state-" (state) } if state] data-size="compact" if compact ..rest {
            ol class="mk-track" aria-label=(label) {
                @for step in steps {
                    li class={ "step-" (step.progress) } {
                        span class="mk-track-dot" aria-hidden="true" {
                            @if step.progress == StepProgress::Done {
                                svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3"
                                    stroke-linecap="round" stroke-linejoin="round" { path d="m4.5 12.5 5 5 10-11" {} }
                            } @else if step.progress == StepProgress::Fail {
                                svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3"
                                    stroke-linecap="round" { path d="M7 7l10 10M17 7 7 17" {} }
                            }
                        }
                        span class="mk-track-name" {
                            (step.label)
                            @if step.progress == StepProgress::Done { span class="visually-hidden" { ", done" } }
                            @else if step.progress == StepProgress::Now { span class="visually-hidden" { ", now" } }
                            @else if step.progress == StepProgress::Part { span class="visually-hidden" { ", partly done" } }
                            @else if step.progress == StepProgress::Fail { span class="visually-hidden" { ", stopped" } }
                            @else { span class="visually-hidden" { ", to come" } }
                        }
                    }
                }
            }
            p class="mk-stage-msg" role="status" { strong { (title) "." } " " (message) }
        }
    }
}

pub(super) fn progress_steps_examples() -> Vec<(&'static str, ProgressSteps)> {
    use crate::OrderState;
    let step = |label: &str, progress| ProgressStep {
        label: label.into(),
        progress,
    };
    let payment = |steps: Vec<ProgressStep>, title: &str, message: &str, state| {
        ProgressSteps::new("Payment progress", steps, title, message).state(state)
    };
    use StepProgress::*;
    vec![
        (
            "Waiting for a payment",
            payment(
                vec![step("Send", Now), step("Confirm", Todo), step("Paid", Todo)],
                "Send 0.42 XMR",
                "Scan the code or copy the address. 58 min left",
                OrderState::Pending,
            ),
        ),
        (
            "Part paid",
            payment(
                vec![step("Send", Part), step("Confirm", Todo), step("Paid", Todo)],
                "Part paid",
                "0.2 XMR in; send the other 0.22 XMR.",
                OrderState::Partial,
            ),
        ),
        (
            "Confirming",
            payment(
                vec![step("Send", Done), step("Confirm", Now), step("Paid", Todo)],
                "Confirming",
                "Payment seen; 3 of 10 confirmations.",
                OrderState::Confirming,
            ),
        ),
        (
            "Paid, counted as soon as seen",
            payment(
                vec![step("Send", Done), step("Paid", Done)],
                "Paid",
                "Thank you.",
                OrderState::Paid,
            ),
        ),
        (
            "Expired",
            payment(
                vec![step("Send", Fail), step("Confirm", Todo), step("Paid", Todo)],
                "Expired",
                "Nothing arrived in time; ask for a new order.",
                OrderState::Expired,
            ),
        ),
        (
            "On the POS",
            payment(
                vec![step("Send", Now), step("Confirm", Todo), step("Paid", Todo)],
                "Send 0.42 XMR",
                "Scan the code.",
                OrderState::Pending,
            )
            .compact(true),
        ),
    ]
}
