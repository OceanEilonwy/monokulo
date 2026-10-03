//! A network's history for the engine page: the engine's events, the state
//! machine run over them with a keyframe every [`KEYFRAME_EVERY_MS`], and
//! the marks. It answers the live state, the state at any moment (the
//! timeline's scrubbing) and the frames between two moments (replay).
//!
//! No I/O: the relay (`engine_view::relay`) feeds it the engine's pages.

use std::collections::VecDeque;

use serde::Serialize;
use shared::activity::{ActivityPage, Event, Recorded, Tuning};

use super::machine::{step, Effect, Mark, State};
use super::present::{present, present_round, Presented, RoundView};

/// How far back the history reaches: the engine keeps as much.
pub const KEEP_MS: i64 = 30 * 60_000;
/// How often the state is kept, so a moment is rebuilt from at most this
/// much of events.
pub const KEYFRAME_EVERY_MS: i64 = 5_000;
/// Replay's frames are this far apart at most.
pub const FRAME_MS: i64 = 500;
/// The longest stretch one replay request covers.
pub const MAX_REPLAY_MS: i64 = 60_000;

pub struct History {
    network: String,
    epoch: String,
    tuning: Tuning,
    events: VecDeque<Recorded>,
    keyframes: VecDeque<Keyframe>,
    marks: VecDeque<Mark>,
    live: State,
    next: u64,
    engine_now_ms: i64,
}

/// The state just after the event before `next_seq`.
#[derive(Clone)]
struct Keyframe {
    at_ms: i64,
    next_seq: u64,
    state: State,
}

/// What the page draws at one moment, and what led there.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Frame {
    pub at_ms: i64,
    pub view: Presented,
    pub effects: Vec<Timed>,
    pub marks: Vec<Mark>,
}

/// An effect at the time of the event that caused it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Timed {
    pub at_ms: i64,
    pub effect: Effect,
}

/// What a page from the engine did to the history.
#[derive(Debug)]
pub enum Update {
    /// New events: what the page draws now, and what led there.
    Frame(Box<Frame>),
    /// Nothing new.
    Nothing,
    /// The engine restarted, or the history fell behind its record: the
    /// history started over, and viewers should too.
    Restarted,
}

impl History {
    /// A history from the engine's first page (every event from its oldest
    /// snapshot).
    pub fn new(page: ActivityPage) -> Self {
        let first = page.events.first().map_or(page.next, |e| e.seq);
        let mut history = Self {
            network: page.network,
            epoch: page.epoch,
            tuning: page.tuning,
            events: VecDeque::new(),
            keyframes: VecDeque::from([Keyframe {
                at_ms: i64::MIN,
                next_seq: first,
                state: State::default(),
            }]),
            marks: VecDeque::new(),
            live: State::default(),
            next: first,
            engine_now_ms: page.now_ms,
        };
        let _ = history.apply(page.events);
        history.next = page.next;
        history
    }

    /// Takes the engine's next page (asked for from [`Self::next`]).
    pub fn update(&mut self, page: ActivityPage) -> Update {
        if page.epoch != self.epoch || page.gap {
            *self = Self::new(page);
            return Update::Restarted;
        }
        self.engine_now_ms = page.now_ms;
        self.tuning = page.tuning;
        self.next = self.next.max(page.next);
        let (applied, effects, marks) = self.apply(page.events);
        if applied == 0 {
            return Update::Nothing;
        }
        Update::Frame(Box::new(Frame {
            at_ms: self.live.at_ms,
            view: present(&self.live, &self.tuning),
            effects,
            marks,
        }))
    }

    /// Runs the machine over `events` (those not seen yet), keeping
    /// keyframes and marks, and lets go of what is older than [`KEEP_MS`].
    /// Returns how many were new, and their effects and marks.
    fn apply(&mut self, events: Vec<Recorded>) -> (usize, Vec<Timed>, Vec<Mark>) {
        let mut applied = 0;
        let mut effects = Vec::new();
        let mut marks = Vec::new();
        let newest = self.events.back().map(|e| e.seq);
        for recorded in events {
            if newest.is_some_and(|newest| recorded.seq <= newest) {
                continue;
            }
            applied += 1;
            let output = step(&mut self.live, &recorded);
            effects.extend(output.effects.into_iter().map(|effect| Timed {
                at_ms: recorded.at_ms,
                effect,
            }));
            if let Some(mark) = output.mark {
                self.marks.push_back(mark.clone());
                marks.push(mark);
            }
            let keyframe_due = self
                .keyframes
                .back()
                .is_none_or(|last| recorded.at_ms.saturating_sub(last.at_ms) >= KEYFRAME_EVERY_MS);
            if keyframe_due {
                self.keyframes.push_back(Keyframe {
                    at_ms: recorded.at_ms,
                    next_seq: recorded.seq + 1,
                    state: self.live.clone(),
                });
            }
            self.events.push_back(recorded);
        }
        self.forget_before(self.live.at_ms.saturating_sub(KEEP_MS));
        (applied, effects, marks)
    }

    /// Lets go of what happened before `cutoff`, keeping the newest
    /// keyframe from before it as the base everything later is rebuilt
    /// from.
    fn forget_before(&mut self, cutoff: i64) {
        while self.keyframes.len() > 1 && self.keyframes[1].at_ms <= cutoff {
            self.keyframes.pop_front();
        }
        let base = self.keyframes.front().map_or(0, |k| k.next_seq);
        while self.events.front().is_some_and(|e| e.seq < base) {
            self.events.pop_front();
        }
        while self.marks.front().is_some_and(|m| m.at_ms < cutoff) {
            self.marks.pop_front();
        }
    }

    /// The sequence number to ask the engine for next.
    pub fn next(&self) -> u64 {
        self.next
    }

    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    pub fn network(&self) -> &str {
        &self.network
    }

    /// The engine's clock as of its last page.
    pub fn engine_now_ms(&self) -> i64 {
        self.engine_now_ms
    }

    /// The oldest moment the history can show.
    pub fn oldest_ms(&self) -> Option<i64> {
        self.events.front().map(|e| e.at_ms)
    }

    /// The live state.
    pub fn live(&self) -> &State {
        &self.live
    }

    /// What the page draws now.
    pub fn live_frame(&self) -> Frame {
        Frame {
            at_ms: self.live.at_ms,
            view: present(&self.live, &self.tuning),
            effects: Vec::new(),
            marks: Vec::new(),
        }
    }

    /// Every mark kept, oldest first.
    pub fn marks(&self) -> impl DoubleEndedIterator<Item = &Mark> {
        self.marks.iter()
    }

    /// The state as it was at `at_ms`: after every event up to then.
    pub fn state_at(&self, at_ms: i64) -> State {
        let index = self
            .keyframes
            .iter()
            .rposition(|k| k.at_ms <= at_ms)
            .unwrap_or(0);
        let keyframe = &self.keyframes[index];
        let mut state = keyframe.state.clone();
        for recorded in self
            .events
            .iter()
            .skip_while(|e| e.seq < keyframe.next_seq)
            .take_while(|e| e.at_ms <= at_ms)
        {
            step(&mut state, recorded);
        }
        state
    }

    /// Round `number` as the round card draws it once it ended (or as far
    /// as it has got): `None` once it has left the history.
    pub fn round(&self, number: u64) -> Option<RoundView> {
        let starts = |recorded: &Recorded| matches!(recorded.event, Event::RoundStarted { round, budget_ms: _, tip: _ } if round == number);
        let start = self.events.iter().find(|recorded| starts(recorded))?.seq;
        let index = self.keyframes.iter().rposition(|k| k.next_seq <= start)?;
        let keyframe = &self.keyframes[index];
        let mut state = keyframe.state.clone();
        for recorded in self.events.iter().skip_while(|e| e.seq < keyframe.next_seq) {
            if recorded.seq > start
                && matches!(
                    recorded.event,
                    Event::RoundStarted {
                        round: _,
                        budget_ms: _,
                        tip: _
                    }
                )
            {
                break;
            }
            step(&mut state, recorded);
            if matches!(recorded.event, Event::RoundFinished { round, ms: _, backlogged: _ } if round == number)
            {
                break;
            }
        }
        present_round(&state, &self.tuning)
    }

    /// What the page draws at `at_ms`.
    pub fn frame_at(&self, at_ms: i64) -> Frame {
        Frame {
            at_ms,
            view: present(&self.state_at(at_ms), &self.tuning),
            effects: Vec::new(),
            marks: Vec::new(),
        }
    }

    /// Replay from `from_ms` to `to_ms` (at most [`MAX_REPLAY_MS`] of it):
    /// a frame at the end of each [`FRAME_MS`] that had events, with the
    /// effects and marks of its events.
    pub fn frames(&self, from_ms: i64, to_ms: i64) -> Vec<Frame> {
        let to_ms = to_ms.min(from_ms.saturating_add(MAX_REPLAY_MS));
        let mut state = self.state_at(from_ms);
        let mut frames: Vec<Frame> = Vec::new();
        let mut window: Option<(i64, Vec<Timed>, Vec<Mark>)> = None;
        for recorded in self
            .events
            .iter()
            .filter(|e| e.at_ms > from_ms && e.at_ms <= to_ms)
        {
            // Saturating: `from_ms` is whatever the request said.
            let window_end = from_ms.saturating_add(
                recorded
                    .at_ms
                    .saturating_sub(from_ms)
                    .saturating_add(FRAME_MS - 1)
                    / FRAME_MS
                    * FRAME_MS,
            );
            if window
                .as_ref()
                .is_some_and(|(end, _, _)| *end != window_end)
            {
                if let Some((end, effects, marks)) = window.take() {
                    frames.push(self.frame(end.min(to_ms), &state, effects, marks));
                }
            }
            let (_, effects, marks) =
                window.get_or_insert_with(|| (window_end, Vec::new(), Vec::new()));
            let output = step(&mut state, recorded);
            effects.extend(output.effects.into_iter().map(|effect| Timed {
                at_ms: recorded.at_ms,
                effect,
            }));
            marks.extend(output.mark);
        }
        if let Some((end, effects, marks)) = window {
            frames.push(self.frame(end.min(to_ms), &state, effects, marks));
        }
        frames
    }

    fn frame(&self, at_ms: i64, state: &State, effects: Vec<Timed>, marks: Vec<Mark>) -> Frame {
        Frame {
            at_ms,
            view: present(state, &self.tuning),
            effects,
            marks,
        }
    }
}

#[cfg(test)]
mod tests;
