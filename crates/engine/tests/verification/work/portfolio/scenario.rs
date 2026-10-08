//! Portfolio histories: setup bytes, then typed commands, decoded from any
//! bytes (so the fuzzer's mutations are all histories). Setup bytes never
//! consume command bytes. Bit 1 of setup byte 3 (bit 0 picks the worker
//! backend) starts every transaction observed in the pool.
pub(crate) const SETUP_BYTES: usize = 128;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Arrive(u8),
    Mine(u8),
    Extend(u8),
    Reorg(u8),
    Drop(u8),
    Spent {
        transaction: u8,
        unanimous: bool,
    },
    Proof {
        lag: u8,
        mismatch: bool,
    },
    Advance(u8),
    Restart,
    Fault {
        writes: bool,
        position: u8,
    },
    Deliver(bool),
    Round,
    /// Replaces every block from height 3 with a new branch of 4 (or, with
    /// `place` 3, 4 + `slot`) blocks, mined transactions staying at their
    /// heights. Before that, `place` puts `transaction` in the block at
    /// 3 + `slot` (0), the pool (1) or nowhere (2), or leaves it (3).
    Rebuild {
        transaction: u8,
        place: u8,
        slot: u8,
    },
    /// Two fast pool passes.
    FastPass,
    /// The wallet's custody handle replaced by a new registration.
    ReplaceCustody(u8),
}
impl Command {
    pub(crate) const fn family(&self) -> &'static str {
        match self {
            Self::Arrive(_) => "Arrive",
            Self::Mine(_) => "Mine",
            Self::Extend(_) => "Extend",
            Self::Reorg(_) => "Reorg",
            Self::Drop(_) => "Drop",
            Self::Spent {
                transaction: _,
                unanimous: _,
            } => "Spent",
            Self::Proof {
                lag: _,
                mismatch: _,
            } => "Proof",
            Self::Advance(_) => "Advance",
            Self::Restart => "Restart",
            Self::Fault {
                writes: _,
                position: _,
            } => "Fault",
            Self::Deliver(_) => "Deliver",
            Self::Round => "Round",
            Self::Rebuild {
                transaction: _,
                place: _,
                slot: _,
            } => "Rebuild",
            Self::FastPass => "FastPass",
            Self::ReplaceCustody(_) => "ReplaceCustody",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Scenario {
    pub(crate) setup: Vec<u8>,
    pub(crate) commands: Vec<Command>,
}
impl Scenario {
    pub(crate) fn decode(data: &[u8]) -> Self {
        let mut setup = vec![0; SETUP_BYTES];
        for (out, value) in setup.iter_mut().zip(data) {
            *out = *value;
        }
        let commands = data
            .get(SETUP_BYTES..)
            .unwrap_or_default()
            .chunks(4)
            .take(32)
            .map(|c| {
                let a = c.get(1).copied().unwrap_or_default();
                let b = c.get(2).copied().unwrap_or_default();
                match c[0] % 15 {
                    0 => Command::Arrive(a),
                    1 => Command::Mine(a),
                    2 => Command::Extend(a),
                    3 => Command::Reorg(a),
                    4 => Command::Drop(a),
                    5 => Command::Spent {
                        transaction: a,
                        unanimous: b & 1 != 0,
                    },
                    6 => Command::Proof {
                        lag: a,
                        mismatch: b & 1 != 0,
                    },
                    7 => Command::Advance(a),
                    8 => Command::Restart,
                    9 => Command::Fault {
                        writes: b & 1 != 0,
                        position: a % 4,
                    },
                    10 => Command::Deliver(a & 1 != 0),
                    11 => Command::Round,
                    12 => Command::Rebuild {
                        transaction: a,
                        place: b % 4,
                        slot: (b >> 2) % 4,
                    },
                    13 => Command::FastPass,
                    _ => Command::ReplaceCustody(a),
                }
            })
            .collect();
        Self { setup, commands }
    }
    #[cfg(test)]
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut bytes: Vec<u8> = self
            .setup
            .iter()
            .copied()
            .chain(std::iter::repeat(0))
            .take(SETUP_BYTES)
            .collect();
        for command in self.commands.iter().take(32) {
            let (op, a, b) = match *command {
                Command::Arrive(t) => (0, t, 0),
                Command::Mine(t) => (1, t, 0),
                Command::Extend(n) => (2, n, 0),
                Command::Reorg(n) => (3, n, 0),
                Command::Drop(t) => (4, t, 0),
                Command::Spent {
                    transaction,
                    unanimous,
                } => (5, transaction, u8::from(unanimous)),
                Command::Proof { lag, mismatch } => (6, lag, u8::from(mismatch)),
                Command::Advance(n) => (7, n, 0),
                Command::Restart => (8, 0, 0),
                Command::Fault { writes, position } => (9, position, u8::from(writes)),
                Command::Deliver(f) => (10, u8::from(f), 0),
                Command::Round => (11, 0, 0),
                Command::Rebuild {
                    transaction,
                    place,
                    slot,
                } => (12, transaction, (place % 4) | ((slot % 4) << 2)),
                Command::FastPass => (13, 0, 0),
                Command::ReplaceCustody(w) => (14, w, 0),
            };
            bytes.extend([op, a, b, 0]);
        }
        bytes
    }
}
#[cfg(test)]
pub(crate) fn strategy() -> impl proptest::strategy::Strategy<Value = Scenario> {
    use proptest::prelude::*;
    let command = prop_oneof![
        any::<u8>().prop_map(Command::Arrive),
        any::<u8>().prop_map(Command::Mine),
        (0u8..4).prop_map(Command::Extend),
        (0u8..4).prop_map(Command::Reorg),
        any::<u8>().prop_map(Command::Drop),
        (any::<u8>(), any::<bool>()).prop_map(|(transaction, unanimous)| Command::Spent {
            transaction,
            unanimous
        }),
        (0u8..8, any::<bool>()).prop_map(|(lag, mismatch)| Command::Proof { lag, mismatch }),
        (0u8..8).prop_map(Command::Advance),
        Just(Command::Restart),
        (any::<bool>(), 0u8..4).prop_map(|(writes, position)| Command::Fault { writes, position }),
        any::<bool>().prop_map(Command::Deliver),
        Just(Command::Round),
        (any::<u8>(), 0u8..4, 0u8..4).prop_map(|(transaction, place, slot)| Command::Rebuild {
            transaction,
            place,
            slot
        }),
        Just(Command::FastPass),
        any::<u8>().prop_map(Command::ReplaceCustody),
    ];
    (
        prop::collection::vec(any::<u8>(), SETUP_BYTES),
        prop::collection::vec(command, 0..17),
    )
        .prop_map(|(setup, commands)| Scenario { setup, commands })
}

/// Fixed histories the positive-control tests run and the fuzzer starts
/// from (`fuzz/seeds/portfolio/<name>`, each the encoding of its scenario).
#[cfg(test)]
pub(crate) mod reviewed {
    use super::{Command::*, Scenario, SETUP_BYTES};

    /// Bit 1 of setup byte 3: every transaction observed in the pool first.
    const POOL_FIRST: u8 = 2;

    fn rebuild(transaction: u8, place: u8, slot: u8) -> super::Command {
        Rebuild {
            transaction,
            place,
            slot,
        }
    }

    /// Two wallets, two transactions with one extra output each, every
    /// invoice's goal `goal` (0 exact, 1 half, 2 one more than paid) and
    /// threshold one. Forces missing and mismatching proof, mined
    /// transactions surviving rebuilt branches, disputed and unanimous
    /// spent votes, fast passes, SQL faults with custody replacement, a
    /// restart, and the voided transaction restored to a canonical block.
    pub(crate) fn combined(worker: u8, goal: u8) -> Scenario {
        let mut setup = vec![0; SETUP_BYTES];
        setup[3] = worker | POOL_FIRST;
        // Invoice bytes (goal, threshold, expiry) follow the outputs' 16.
        for invoice in 0..4 {
            setup[16 + 3 * invoice] = goal;
            setup[17 + 3 * invoice] = 1;
        }
        Scenario {
            setup,
            commands: vec![
                rebuild(0, 0, 0),
                Proof {
                    lag: 0,
                    mismatch: true,
                },
                rebuild(1, 0, 1),
                Proof {
                    lag: 0,
                    mismatch: false,
                },
                rebuild(0, 2, 0),
                Spent {
                    transaction: 0,
                    unanimous: false,
                },
                Spent {
                    transaction: 0,
                    unanimous: true,
                },
                rebuild(0, 3, 0),
                FastPass,
                Fault {
                    writes: false,
                    position: 0,
                },
                ReplaceCustody(1),
                Fault {
                    writes: true,
                    position: 0,
                },
                ReplaceCustody(1),
                Restart,
                rebuild(0, 0, 2),
                Proof {
                    lag: 0,
                    mismatch: false,
                },
            ],
        }
    }

    /// The frozen paying `RingCT` transaction (variant `goal`), a generated
    /// payment and the recorded foreign transaction `foreign`, with whole
    /// or pruned daemon bodies; thresholds `1 + goal`.
    pub(crate) fn recorded(worker: u8, pruned: bool, foreign: u8, goal: u8) -> Scenario {
        let mut setup = vec![0; SETUP_BYTES];
        // Bit 7 the recorded corpus, bit 6 pruned bodies; 129 makes two
        // wallets and 193 three.
        setup[0] = if pruned { 193 } else { 129 };
        let wallets = if pruned { 3 } else { 2 };
        setup[2] = goal;
        setup[3] = worker | POOL_FIRST;
        // The generated payment's bytes, then the foreign transaction's,
        // then the invoices'.
        setup[9 + wallets] = foreign;
        for invoice in 0..2 * wallets {
            setup[10 + wallets + 3 * invoice] = goal;
            setup[11 + wallets + 3 * invoice] = 1 + goal;
        }
        Scenario {
            setup,
            commands: vec![
                rebuild(0, 0, 0),
                rebuild(1, 0, 1),
                rebuild(2, 0, 2),
                Proof {
                    lag: 0,
                    mismatch: false,
                },
                rebuild(0, 2, 0),
                Spent {
                    transaction: 0,
                    unanimous: false,
                },
                Spent {
                    transaction: 0,
                    unanimous: true,
                },
                Restart,
                Fault {
                    writes: false,
                    position: 0,
                },
                ReplaceCustody(1),
                rebuild(0, 0, 1),
                Proof {
                    lag: 0,
                    mismatch: false,
                },
                Deliver(true),
            ],
        }
    }

    /// Every reviewed seed, by file name.
    pub(crate) fn seeds() -> Vec<(String, Scenario)> {
        let mut seeds = vec![
            (
                "empty".to_owned(),
                Scenario {
                    setup: vec![0; SETUP_BYTES],
                    commands: vec![],
                },
            ),
            ("partial-payments".to_owned(), combined(1, 1)),
        ];
        for worker in 0..=1 {
            seeds.push((format!("combined-worker-{worker}"), combined(worker, 0)));
            for pruned in [false, true] {
                for foreign in 0..3 {
                    seeds.push((
                        format!(
                            "recorded-{foreign}-pruned-{}-worker-{worker}",
                            u8::from(pruned)
                        ),
                        recorded(worker, pruned, foreign, foreign),
                    ));
                }
            }
        }
        seeds
    }
}
