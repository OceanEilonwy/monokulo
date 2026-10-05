use super::staged_reorg_cleanup;
proptest::proptest! {
    #![proptest_config(persisted_config(crate::property_support::config()))]
    #[test]
    fn generated_reorg_staging_is_scoped_and_durable(net in 0usize..3, fork in 1u64..2001, restart in proptest::prelude::any::<bool>()) {
        staged_reorg_cleanup([monero::Network::Mainnet,monero::Network::Testnet,monero::Network::Stagenet][net],fork,restart);
    }
}

fn persisted_config(config: proptest::test_runner::Config) -> proptest::test_runner::Config {
    crate::property_support::persist(
        config,
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/store/work.txt"
        ),
    )
}
