//! A wallet made in the merchant's browser (`wallet-setup`, monokulo's
//! "Create a new wallet") restores in a real wallet library to the keys
//! monokulo registers for it.

const BIRTHDAY: u64 = 1_791_400_000; // 2026-10-07

fn sample(seed_byte: u8, network: wallet_setup::Network) -> wallet_setup::NewWallet {
    wallet_setup::generate([seed_byte; 32], BIRTHDAY, network).unwrap()
}

#[test]
fn the_polyseed_restores_to_the_address_and_view_key_it_came_with() {
    for byte in [0u8, 7, 0xff] {
        let wallet = sample(byte, wallet_setup::Network::Mainnet);
        assert_eq!(wallet.phrase.split(' ').count(), 16);
        let restored =
            cli_wallet::credentials_from_seed(cli_wallet::Network::Mainnet, &wallet.phrase)
                .unwrap();
        assert_eq!(restored.address, wallet.address);
        assert_eq!(restored.private_view_key_hex, *wallet.view_key_hex);
    }
}

/// The Monero GUI's 25 words open the same wallet.
#[test]
fn the_25_word_phrase_is_the_same_wallet() {
    let wallet = sample(9, wallet_setup::Network::Mainnet);
    assert_eq!(wallet.legacy_phrase.split(' ').count(), 25);
    let restored =
        cli_wallet::credentials_from_seed(cli_wallet::Network::Mainnet, &wallet.legacy_phrase)
            .unwrap();
    assert_eq!(restored.address, wallet.address);
}

#[test]
fn a_stagenet_wallet_restores_on_stagenet() {
    let wallet = sample(5, wallet_setup::Network::Stagenet);
    let restored =
        cli_wallet::credentials_from_seed(cli_wallet::Network::Stagenet, &wallet.phrase).unwrap();
    assert_eq!(restored.address, wallet.address);
}
