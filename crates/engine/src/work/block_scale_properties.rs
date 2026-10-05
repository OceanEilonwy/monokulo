//! Production-sized accounting, anchor preservation, replacement and eviction.
use super::*;
fn header(height: u64) -> ChainBlock {
    ChainBlock {
        height,
        hash: format!("block-{height}"),
        prev_hash: format!("block-{}", height - 1),
        timestamp: height,
        txs: vec![],
        txids: vec![],
        wire_bytes: 512,
    }
}
#[test]
fn thousands_of_cached_headers_obey_the_budget_and_anchor_exception() {
    let mut cache = BlockCache::default();
    for height in 1..=8193 {
        cache.insert(header(height), 512);
        if height % 2 == 0 {
            cache.mark_scanned(height);
        }
    }
    let initial = cache.bytes;
    let discarded = cache.trim(Some(4096), budget_bytes(1));
    assert!(cache.contains(4096));
    assert!(cache.bytes <= budget_bytes(1));
    assert_eq!(
        cache.bytes,
        cache.blocks.values().map(|b| b.bytes).sum::<usize>()
    );
    assert!(discarded <= initial as u64);
    assert!(
        cache.blocks.iter().filter(|(_, b)| b.scanned).count() <= 1,
        "unscanned blocks were evicted before disposable scanned blocks"
    );
    cache.insert(header(4096), 2 * budget_bytes(1));
    cache.trim(Some(4096), budget_bytes(1));
    assert_eq!(
        cache.blocks.len(),
        1,
        "only an oversized pinned block may exceed the budget"
    );
    assert_eq!(cache.bytes, 2 * budget_bytes(1));
    cache.retain(5000..=6000);
    assert_eq!(cache.bytes, 0);
    for height in 5000..=6000 {
        cache.insert(header(height), 512);
    }
    assert_eq!(cache.clear(), 1001 * 512);
    assert_eq!(cache.bytes, 0);
}
