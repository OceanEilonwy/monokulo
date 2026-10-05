use super::*;
use proptest::prelude::*;

proptest! {
    #![proptest_config(crate::property_support::config())]
    #[test]
    fn application_input_boundaries_handle_arbitrary_bytes(data in prop::collection::vec(any::<u8>(), 0..4096)) { inputs(&data); }
    #[test]
    fn event_byte_histories_preserve_scheduler_invariants(data in prop::collection::vec(any::<u8>(), 0..4096)) { schedule(&data); }
    #[test]
    fn sizing_and_retry_boundaries_handle_all_float_bit_patterns(values in any::<[u64;8]>()) { resources(values); }
    #[test]
    fn cpu_ranges_match_an_independent_set_model(ranges in prop::collection::vec((0usize..2048,0usize..32), 0..64)) {
        let mut expected = std::collections::BTreeSet::new();
        let mut parts = Vec::new();
        for (first,length) in ranges { parts.push(format!("{first}-{}",first+length)); expected.extend(first..=first+length); }
        let actual = crate::threads::parse_cpu_list(&parts.join(","));
        if expected.len()>1024 { prop_assert!(actual.is_err()); }
        else { prop_assert_eq!(actual.unwrap(),expected.into_iter().collect::<Vec<_>>()); }
    }
    #[test]
    fn hostile_cpu_range_lengths_are_rejected_before_allocation(first in any::<usize>()) {
        let end = first.saturating_add(1024);
        if end-first>=1024 { let range = format!("{first}-{end}"); prop_assert!(crate::threads::parse_cpu_list(&range).is_err()); }
    }
}

#[test]
fn maximum_cpu_range_is_rejected_without_expansion() {
    crate::threads::parse_cpu_list(&format!("0-{}", usize::MAX)).unwrap_err();
    assert_eq!(
        crate::threads::parse_cpu_list("0-1023,0-1023")
            .unwrap()
            .len(),
        1024
    );
    crate::threads::parse_cpu_list("0-1024").unwrap_err();
}

#[test]
fn special_float_patterns_are_always_exercised() {
    for value in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        -0.0,
        0.0,
        f64::MAX,
        f64::MIN,
        f64::MIN_POSITIVE,
    ] {
        resources([
            u64::MAX,
            u64::MAX,
            value.to_bits(),
            value.to_bits(),
            value.to_bits(),
            value.to_bits(),
            value.to_bits(),
            u64::MAX,
        ]);
    }
}

proptest! {
    #![proptest_config(crate::property_support::config())]
    #[test]
    fn queue_byte_histories_preserve_dispatch_and_drain_contracts(data in prop::collection::vec(any::<u8>(),0..4096)) { queue(&data); }
}
