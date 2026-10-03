//! The history against brute force: whatever moment is asked for, and
//! however much was let go of, it answers what stepping every event up to
//! that moment from the start gives.

use shared::activity::{
    ActivityPage, Event, Group, PoolPath, Recorded, Snapshot, StoreGroup, Tier, Tuning,
    UnitProgress, Wake,
};

use super::*;
use crate::engine_view::machine::{step, State};

const TUNING: Tuning = Tuning {
    round_ms: 10_000,
    shares: [20, 40, 15, 20, 5],
    group_page: 256,
    blocks_per_unit: 8,
    reorg_check_depth: 20,
    poll_ms: 1_000,
};

/// A network over `minutes`: a snapshot every 10 s, a round a second with
/// its units, a new block every 2 minutes committed by the frontier, a
/// fast pass that finds something now and then.
fn network(minutes: i64) -> Vec<Recorded> {
    let mut events = Vec::new();
    let mut seq = 0;
    let mut push = |at_ms: i64, event: Event| {
        events.push(Recorded { seq, at_ms, event });
        seq += 1;
    };
    let start = 1_700_000_000_000;
    let mut tip = 1_000;
    for second in 0..minutes * 60 {
        let at = start + second * 1_000;
        if second % 10 == 0 {
            push(
                at,
                Event::Snapshot(Box::new(Snapshot {
                    tip: Some(tip),
                    high_water: Some(tip),
                    groups: vec![StoreGroup {
                        cursor: tip,
                        stores: 5,
                    }],
                    ..Snapshot::default()
                })),
            );
        }
        let round = u64::try_from(second).unwrap() + 1;
        let new_block = second % 120 == 60;
        if new_block {
            tip += 1;
        }
        push(
            at + 1,
            Event::RoundStarted {
                round,
                budget_ms: 10_000,
                tip: Some(tip),
            },
        );
        push(
            at + 2,
            Event::ChainChecked {
                agrees: true,
                looked_up: true,
            },
        );
        for (i, tier) in Tier::ALL.iter().enumerate() {
            push(
                at + 3 + i as i64,
                Event::Unit {
                    tier: *tier,
                    pass: 1,
                    start_ms: i as u64,
                    ms: 1,
                    progress: UnitProgress::Idle,
                },
            );
        }
        if new_block {
            push(
                at + 9,
                Event::Committed {
                    height: tip,
                    group: Group::Frontier,
                    stores: 5,
                    matches: u64::from(second % 240 == 60),
                    idle_moved: 0,
                    header_only: false,
                },
            );
        }
        push(
            at + 10,
            Event::RoundFinished {
                round,
                ms: 10,
                backlogged: false,
            },
        );
        push(
            at + 11,
            Event::Slept {
                ms: 989,
                woken_by: if new_block {
                    Wake::NewBlock
                } else {
                    Wake::Interval
                },
            },
        );
        if second % 7 == 0 {
            push(
                at + 500,
                Event::PoolScanned {
                    path: PoolPath::Fast,
                    pool: 3,
                    scanned: 1,
                },
            );
        }
    }
    events
}

fn page(events: &[Recorded], epoch: &str) -> ActivityPage {
    ActivityPage {
        network: "stagenet".into(),
        epoch: epoch.into(),
        now_ms: events.last().map_or(0, |e| e.at_ms),
        tuning: TUNING,
        events: events.to_vec(),
        next: events.last().map_or(0, |e| e.seq + 1),
        gap: false,
    }
}

/// Stepping every event up to `at_ms` from the start.
fn brute_force(events: &[Recorded], at_ms: i64) -> State {
    let mut state = State::default();
    for recorded in events.iter().take_while(|e| e.at_ms <= at_ms) {
        step(&mut state, recorded);
    }
    state
}

/// Any moment of a history read in one page, and in many, is what stepping
/// the events gives; the live state is the last of them.
#[test]
fn the_state_at_any_moment_is_what_the_events_give() {
    let events = network(12);
    let whole = History::new(page(&events, "e1"));
    let mut pieces = History::new(page(&events[..100], "e1"));
    for chunk in events[100..].chunks(37) {
        let _ = pieces.update(page(chunk, "e1"));
    }
    assert_eq!(whole.live(), &brute_force(&events, i64::MAX));
    assert_eq!(pieces.live(), whole.live());
    let first = events[0].at_ms;
    for offset in [
        0, 1, 999, 4_999, 5_000, 61_003, 300_000, 431_207, 719_999, 10_000_000,
    ] {
        let at = first + offset;
        let expected = brute_force(&events, at);
        assert_eq!(whole.state_at(at), expected, "at +{offset} ms");
        assert_eq!(
            pieces.state_at(at),
            expected,
            "at +{offset} ms, read in pieces"
        );
    }
    assert_eq!(whole.next(), events.last().unwrap().seq + 1);
}

/// Over half an hour old is let go of; what is kept still rebuilds exactly,
/// marks included, and before the oldest moment kept is the oldest state.
#[test]
fn what_is_older_than_the_history_keeps_is_let_go_of_and_the_rest_still_rebuilds() {
    let events = network(45);
    let history = History::new(page(&events, "e1"));
    let newest = events.last().unwrap().at_ms;
    let oldest = history.oldest_ms().unwrap();
    assert!(
        newest - oldest <= KEEP_MS + KEYFRAME_EVERY_MS,
        "{}",
        newest - oldest
    );
    assert!(newest - oldest >= KEEP_MS - KEYFRAME_EVERY_MS);
    for at in [oldest, oldest + 12_345, newest - 60_000, newest] {
        assert_eq!(history.state_at(at), brute_force(&events, at));
    }
    assert!(history.marks().all(|m| m.at_ms >= newest - KEEP_MS));
    assert!(history.marks().count() > 0);
    let keyframes = history.keyframes.len() as i64;
    assert!(
        (KEEP_MS / KEYFRAME_EVERY_MS..=KEEP_MS / KEYFRAME_EVERY_MS + 2).contains(&keyframes),
        "{keyframes}"
    );
}

/// A page of new events is a frame: what the page draws now, the effects
/// at their events' times, the marks; one with nothing new is nothing, and
/// events already applied are not applied twice.
#[test]
fn new_events_make_a_frame_and_old_ones_nothing() {
    let events = network(3);
    let (first, rest) = events.split_at(events.len() - 30);
    let mut history = History::new(page(first, "e1"));
    let Update::Frame(frame) = history.update(page(rest, "e1")) else {
        panic!("new events make a frame");
    };
    let expected = brute_force(&events, i64::MAX);
    assert_eq!(frame.at_ms, expected.at_ms);
    assert_eq!(frame.view, present(&expected, &TUNING));
    let mut stepped = brute_force(first, i64::MAX);
    let mut effects = Vec::new();
    for recorded in rest {
        effects.extend(
            step(&mut stepped, recorded)
                .effects
                .into_iter()
                .map(|effect| Timed {
                    at_ms: recorded.at_ms,
                    effect,
                }),
        );
    }
    assert_eq!(frame.effects, effects);
    assert!(
        matches!(history.update(page(rest, "e1")), Update::Nothing),
        "already applied"
    );
    assert!(matches!(history.update(page(&[], "e1")), Update::Nothing));
    assert_eq!(history.live(), &expected);
}

/// The engine restarted (another epoch), or the history fell behind its
/// record (a gap): the history starts over from the page.
#[test]
fn a_restart_or_a_gap_starts_the_history_over() {
    let events = network(2);
    let mut history = History::new(page(&events, "e1"));
    let fresh = network(1);
    assert!(matches!(
        history.update(page(&fresh, "e2")),
        Update::Restarted
    ));
    assert_eq!(history.epoch(), "e2");
    assert_eq!(history.live(), &brute_force(&fresh, i64::MAX));
    let mut gapped = page(&fresh, "e2");
    gapped.gap = true;
    assert!(matches!(history.update(gapped), Update::Restarted));
    assert_eq!(history.network(), "stagenet");
}

/// Replay: frames at the end of each half second with events, carrying
/// those events' effects and marks; the last frame draws the state at its
/// time; one request covers a minute at most.
#[test]
fn replay_frames_carry_their_events_and_end_on_the_right_state() {
    let events = network(5);
    let history = History::new(page(&events, "e1"));
    let from = events[0].at_ms + 100_000;
    let to = from + 10_000;
    let frames = history.frames(from, to);
    assert!(!frames.is_empty());
    assert!(frames.windows(2).all(|w| w[0].at_ms < w[1].at_ms));
    assert!(frames.iter().all(|f| f.at_ms > from && f.at_ms <= to));
    assert!(frames
        .iter()
        .all(|f| (f.at_ms - from) % FRAME_MS == 0 || f.at_ms == to));
    let last = frames.last().unwrap();
    assert_eq!(
        last.view,
        present(&brute_force(&events, last.at_ms), &TUNING)
    );
    let mut state = brute_force(&events, from);
    let mut effects = Vec::new();
    let mut marks = Vec::new();
    for recorded in events.iter().filter(|e| e.at_ms > from && e.at_ms <= to) {
        let out = step(&mut state, recorded);
        effects.extend(out.effects.into_iter().map(|effect| Timed {
            at_ms: recorded.at_ms,
            effect,
        }));
        marks.extend(out.mark);
    }
    assert_eq!(
        frames
            .iter()
            .flat_map(|f| f.effects.clone())
            .collect::<Vec<_>>(),
        effects
    );
    assert_eq!(
        frames
            .iter()
            .flat_map(|f| f.marks.clone())
            .collect::<Vec<_>>(),
        marks
    );
    let long = history.frames(events[0].at_ms, events[0].at_ms + 10 * MAX_REPLAY_MS);
    assert!(long.last().unwrap().at_ms <= events[0].at_ms + MAX_REPLAY_MS);
    assert_eq!(
        history.frame_at(to).view,
        present(&brute_force(&events, to), &TUNING)
    );
    assert_eq!(history.live_frame().view, present(history.live(), &TUNING));
}

/// A past round, chosen from the recent rounds, is rebuilt as it ended:
/// what stepping every event up to its end gives. Once it has left the
/// history it can't be.
#[test]
fn a_past_round_is_rebuilt_as_it_ended() {
    let events = network(3);
    let history = History::new(page(&events, "e"));
    for number in [1, 5, 120] {
        let finished = events
            .iter()
            .find(|e| matches!(e.event, Event::RoundFinished { round, ms: _, backlogged: _ } if round == number))
            .unwrap();
        let expected = present_round(&brute_force(&events, finished.at_ms), &TUNING);
        assert_eq!(history.round(number), expected, "round {number}");
        assert_eq!(expected.unwrap().number, number);
    }
    assert_eq!(history.round(999), None, "never happened");

    let long = network(40);
    let trimmed = History::new(page(&long, "e"));
    assert_eq!(trimmed.round(1), None, "left the history");
    assert!(trimmed.round(2_390).is_some());
}

/// Replay and scrubbing take any moment a request names, however far off.
#[test]
fn any_moment_asked_for_is_answered() {
    let events = network(1);
    let history = History::new(page(&events, "e"));
    for (from, to) in [
        (i64::MIN, i64::MAX),
        (i64::MIN, 0),
        (i64::MAX - 1, i64::MAX),
    ] {
        let _ = history.frames(from, to);
    }
    let _ = history.frame_at(i64::MIN);
    let _ = history.frame_at(i64::MAX);
}
