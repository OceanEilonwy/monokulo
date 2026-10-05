//! Versioned semantic histories. Setup bytes never consume command bytes.
const MAGIC: &[u8; 4] = b"MKP\x01";
pub(crate) const SETUP_BYTES: usize = 128;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Arrive(u8),
    Mine(u8),
    Extend(u8),
    Reorg(u8),
    Drop(u8),
    Spent { transaction: u8, unanimous: bool },
    Proof { lag: u8, mismatch: bool },
    Advance(u8),
    Restart,
    Fault { writes: bool, position: u8 },
    Deliver(bool),
    Round,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Scenario {
    pub(crate) setup: Vec<u8>,
    pub(crate) commands: Vec<Command>,
}
impl Scenario {
    pub(crate) fn decode(data: &[u8]) -> Option<Self> {
        if !data.starts_with(MAGIC) {
            return None;
        }
        let mut setup = vec![0; SETUP_BYTES];
        for (out, value) in setup.iter_mut().zip(data.iter().skip(4)) {
            *out = *value;
        }
        let commands = data
            .get(4 + SETUP_BYTES..)
            .unwrap_or_default()
            .chunks(4)
            .take(32)
            .map(|c| {
                let a = c.get(1).copied().unwrap_or_default();
                let b = c.get(2).copied().unwrap_or_default();
                match c[0] % 12 {
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
                    _ => Command::Round,
                }
            })
            .collect();
        Some(Self { setup, commands })
    }
    #[cfg(test)]
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        bytes.extend(
            self.setup
                .iter()
                .copied()
                .chain(std::iter::repeat(0))
                .take(SETUP_BYTES),
        );
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
    ];
    (
        prop::collection::vec(any::<u8>(), SETUP_BYTES),
        prop::collection::vec(command, 0..17),
    )
        .prop_map(|(setup, commands)| Scenario { setup, commands })
}
