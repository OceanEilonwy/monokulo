//! The rules against real mainnet blocks (a fixture straddling a RandomX
//! key change), and each rule's edges.

use super::hasher::Hasher;
use super::*;

/// `fixtures/mainnet_3774524.json`: blocks 3774524..=3774533 and the 735
/// before them, as a public node reported them.
struct Fixture {
    /// (height, id, timestamp, cumulative difficulty, difficulty).
    rows: Vec<(u64, [u8; 32], u64, u128, u128)>,
    blobs: std::collections::BTreeMap<u64, Vec<u8>>,
    seeds: std::collections::BTreeMap<u64, [u8; 32]>,
}

fn id(hex_id: &str) -> [u8; 32] {
    hex::decode(hex_id).unwrap().try_into().unwrap()
}

fn fixture() -> Fixture {
    let json: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/mainnet_3774524.json")).unwrap();
    let rows = json["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row[0].as_u64().unwrap(),
                id(row[1].as_str().unwrap()),
                row[2].as_u64().unwrap(),
                row[3].as_str().unwrap().parse().unwrap(),
                row[4].as_str().unwrap().parse().unwrap(),
            )
        })
        .collect();
    let blobs = json["blobs"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(h, blob)| {
            (
                h.parse().unwrap(),
                hex::decode(blob.as_str().unwrap()).unwrap(),
            )
        })
        .collect();
    let seeds = json["seeds"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(h, hash)| (h.parse().unwrap(), id(hash.as_str().unwrap())))
        .collect();
    Fixture { rows, blobs, seeds }
}

impl Fixture {
    fn window_before(&self, height: u64) -> Window {
        Window::new(
            self.rows
                .iter()
                .filter(|r| r.0 < height && r.0 + DIFFICULTY_BLOCKS as u64 >= height)
                .map(
                    |&(height, id, timestamp, cumulative_difficulty, _)| ProvenBlock {
                        height,
                        id,
                        timestamp,
                        cumulative_difficulty,
                    },
                ),
        )
    }

    fn row(&self, height: u64) -> (u64, [u8; 32], u64, u128, u128) {
        *self.rows.iter().find(|r| r.0 == height).unwrap()
    }
}

#[test]
fn real_blocks_difficulty_is_computed_as_monerod_does() {
    let f = fixture();
    for &height in f.blobs.keys() {
        let window = f.window_before(height);
        assert!(window.is_full());
        assert_eq!(window.next_difficulty(), f.row(height).4, "block {height}");
    }
}

#[test]
fn real_blocks_prove_across_a_key_change_and_any_change_fails() {
    let f = fixture();
    let hasher = Hasher::start("test pow").unwrap();
    let mut keys_used = std::collections::BTreeSet::new();
    let mut window = f.window_before(*f.blobs.keys().next().unwrap());
    for (&height, blob) in &f.blobs {
        let candidate = decode(height, blob).unwrap();
        let (_, expected_id, timestamp, cumulative, difficulty) = f.row(height);
        assert_eq!(
            candidate.id, expected_id,
            "the id is computed from the blob"
        );
        assert_eq!(check_header(&window, &candidate), Ok(difficulty));
        assert_eq!(check_time(&candidate, timestamp + 60), Ok(()));
        let key = f.seeds[&seed_height(height)];
        keys_used.insert(seed_height(height));
        let hash = hasher
            .hash_blocking(&key, vec![candidate.pow_input.clone()])
            .unwrap()[0];
        let proven = accept(&window, &candidate, difficulty, &hash).unwrap();
        assert_eq!(proven.cumulative_difficulty, cumulative);

        // Another nonce, another timestamp, one transaction fewer, or the
        // wrong key: none meets the difficulty.
        let mut block: monero::Block = monero::consensus::deserialize(blob).unwrap();
        let mut tampered = Vec::new();
        block.header.nonce ^= 1;
        tampered.push(block.serialize_hashable());
        block.header.nonce ^= 1;
        block.header.timestamp.0 += 1;
        tampered.push(block.serialize_hashable());
        block.header.timestamp.0 -= 1;
        if !block.tx_hashes.is_empty() {
            block.tx_hashes.pop();
            tampered.push(block.serialize_hashable());
        }
        for hash in hasher.hash_blocking(&key, tampered).unwrap() {
            assert!(!check_hash(&hash, difficulty), "block {height} tampered");
        }
        let wrong_key = f.seeds.values().find(|k| **k != key).copied().unwrap();
        let hash = hasher
            .hash_blocking(&wrong_key, vec![candidate.pow_input.clone()])
            .unwrap()[0];
        assert!(matches!(
            accept(&window, &candidate, difficulty, &hash),
            Err(Rejection::ProofOfWork { .. })
        ));
        window.push(proven);
    }
    assert_eq!(keys_used.len(), 2, "the fixture crosses a key change");
    assert!(hasher.stats().keys_built >= 2);
}

#[test]
fn a_real_block_sent_for_another_height_or_garbled_is_refused() {
    let f = fixture();
    let (&height, blob) = f.blobs.iter().next().unwrap();
    assert_eq!(
        decode(height + 1, blob),
        Err(Rejection::WrongHeight {
            height: height + 1,
            sent: height
        })
    );
    assert!(matches!(
        decode(height, &blob[..blob.len() - 1]),
        Err(Rejection::Undecodable { .. })
    ));
    let mut longer = blob.clone();
    longer.push(0);
    assert!(matches!(
        decode(height, &longer),
        Err(Rejection::Undecodable { .. })
    ));
    for rejection in [
        Rejection::WrongHeight { height, sent: 1 },
        Rejection::Undecodable {
            height,
            reason: String::new(),
        },
    ] {
        assert_eq!(rejection.verdict(), Verdict::Invalid);
        assert_eq!(rejection.height(), height);
    }
}

#[test]
fn the_header_rules_each_refuse() {
    let f = fixture();
    let (&height, blob) = f.blobs.iter().next().unwrap();
    let window = f.window_before(height);
    let good = decode(height, blob).unwrap();
    let mut orphan = good.clone();
    orphan.prev_id[0] ^= 1;
    let rejection = check_header(&window, &orphan).unwrap_err();
    assert_eq!(rejection, Rejection::DoesNotFollow { height });
    assert_eq!(rejection.verdict(), Verdict::Moved);

    let mut skipped = good.clone();
    skipped.height += 1;
    assert_eq!(
        check_header(&window, &skipped),
        Err(Rejection::DoesNotFollow { height: height + 1 })
    );

    let mut old = good.clone();
    old.major_version = RANDOMX_MAJOR_VERSION - 1;
    let rejection = check_header(&window, &old).unwrap_err();
    assert!(matches!(rejection, Rejection::NotRandomX { .. }));
    assert_eq!(rejection.verdict(), Verdict::Invalid);

    // Two hours ahead is allowed; a second more waits for the clock.
    assert!(check_time(&good, good.timestamp - FUTURE_TIME_LIMIT_SECS).is_ok());
    let rejection = check_time(&good, good.timestamp - FUTURE_TIME_LIMIT_SECS - 1).unwrap_err();
    assert!(matches!(rejection, Rejection::TimestampInFuture { .. }));
    assert_eq!(rejection.verdict(), Verdict::NotYet);

    let median = window.median_timestamp().unwrap();
    let mut early = good.clone();
    early.timestamp = median;
    assert!(
        check_header(&window, &early).is_ok(),
        "the median itself is allowed"
    );
    early.timestamp = median - 1;
    let rejection = check_header(&window, &early).unwrap_err();
    assert_eq!(
        rejection,
        Rejection::TimestampBeforeMedian {
            height,
            timestamp: median - 1,
            median
        }
    );
    assert_eq!(rejection.verdict(), Verdict::Invalid);

    // No window: nothing to follow.
    assert!(matches!(
        check_header(&Window::default(), &good),
        Err(Rejection::DoesNotFollow { .. })
    ));
}

#[test]
fn seed_heights_change_64_blocks_after_each_2048th() {
    assert_eq!(seed_height(0), 0);
    assert_eq!(seed_height(2048 + 64), 0);
    assert_eq!(seed_height(2048 + 65), 2048);
    assert_eq!(seed_height(2 * 2048 + 64), 2048);
    assert_eq!(seed_height(2 * 2048 + 65), 4096);
    assert_eq!(seed_height(3_774_528), 3_772_416);
    assert_eq!(seed_height(3_774_529), 3_774_464);
}

#[test]
fn a_hash_meets_a_difficulty_when_their_product_fits_in_256_bits() {
    let max = [0xff; 32];
    assert!(check_hash(&max, 1));
    assert!(!check_hash(&max, 2));
    assert!(
        !check_hash(&[0; 32], 0),
        "no difficulty is ever met by zero"
    );
    assert!(check_hash(&[0; 32], u128::MAX));
    // 2^255 - 1 times 2 fits; 2^255 times 2 doesn't.
    let mut below_half = [0xff; 32];
    below_half[31] = 0x7f;
    assert!(check_hash(&below_half, 2));
    let mut half = [0; 32];
    half[31] = 0x80;
    assert!(!check_hash(&half, 2));
    // Difficulties past 64 bits: 2^127 fits against 2^128 (the product is
    // 2^255), 2^129 doesn't.
    let mut two_128 = [0; 32];
    two_128[16] = 1;
    assert!(check_hash(&two_128, 1 << 127));
    let mut two_129 = [0; 32];
    two_129[16] = 2;
    assert!(!check_hash(&two_129, 1 << 127));
    // Carries across limbs.
    let mut low = [0xff; 32];
    low[24..].fill(0);
    assert!(check_hash(&low, u128::from(u64::MAX)));
    assert!(!check_hash(&low, u128::from(u64::MAX) + 2));
}

#[test]
fn the_median_of_an_even_count_is_the_mean_of_the_middle_two() {
    assert_eq!(median(&[]), None);
    assert_eq!(median(&[5]), Some(5));
    assert_eq!(median(&[9, 1, 5]), Some(5));
    assert_eq!(median(&[1, 2, 4, 9]), Some(3));
    assert_eq!(median(&[1, 2, 5, 9]), Some(3), "rounded down");
    assert_eq!(median(&[u64::MAX, u64::MAX]), Some(u64::MAX), "no overflow");
    assert_eq!(median(&[u64::MAX - 1, u64::MAX]), Some(u64::MAX - 1));
}

#[test]
fn difficulty_edges() {
    assert_eq!(next_difficulty(&[]), 1);
    assert_eq!(next_difficulty(&[(10, 5)]), 1);
    // Two blocks a minute apart that did 60 work: 60 × 120 / 60.
    assert_eq!(next_difficulty(&[(0, 0), (60, 60)]), 120);
    // The same time: a span of one second.
    assert_eq!(next_difficulty(&[(7, 0), (7, 3)]), 360);
    // Rounded up.
    assert_eq!(next_difficulty(&[(0, 0), (7, 1)]), 18);
    // Past 64 bits.
    let big = 1u128 << 80;
    assert_eq!(next_difficulty(&[(0, 0), (120, big)]), big);
    // A full window: the newest 15 aren't read, so changing them changes
    // nothing.
    let window: Vec<(u64, u128)> = (0..DIFFICULTY_BLOCKS as u64)
        .map(|i| (i * 120, u128::from(i) * 1000))
        .collect();
    let base = next_difficulty(&window);
    assert_eq!(base, 1000);
    let mut lagged = window.clone();
    for row in lagged.iter_mut().skip(DIFFICULTY_WINDOW) {
        row.0 = 0;
        row.1 = u128::MAX;
    }
    assert_eq!(next_difficulty(&lagged), base);
    // A timestamp far in the future is cut, not counted: the span only
    // loses the slot it left (599 work over 600 slots), where counting it
    // would have sunk the difficulty to almost nothing.
    let mut outlier = window.clone();
    outlier[300].0 = 10_000_000;
    assert_eq!(next_difficulty(&outlier), 999);
}

#[test]
fn the_window_keeps_the_newest_735_and_needs_60_for_a_median() {
    let block = |height: u64| ProvenBlock {
        height,
        id: [0; 32],
        timestamp: height,
        cumulative_difficulty: u128::from(height),
    };
    let mut window = Window::new((0..59).map(block));
    assert_eq!(window.median_timestamp(), None);
    assert!(!window.is_full());
    window.push(block(59));
    assert_eq!(window.median_timestamp(), Some(29));
    let window = Window::new((0..800).map(block));
    assert!(window.is_full());
    assert_eq!(window.tip().unwrap().height, 799);
    assert_eq!(window.median_timestamp(), Some(769));
}

#[test]
fn randomx_starts_at_hard_fork_12_on_each_network() {
    assert_eq!(randomx_fork_height(monero::Network::Mainnet), 1_978_433);
    assert_eq!(randomx_fork_height(monero::Network::Testnet), 1_308_737);
    assert_eq!(randomx_fork_height(monero::Network::Stagenet), 454_721);
}
