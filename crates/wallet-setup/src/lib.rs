//! A new Monero wallet, made where its owner is: in the merchant's browser,
//! as WebAssembly (feature `wasm`), on monokulo's "Create a new wallet" page.
//!
//! [`generate`] turns 32 bytes of the page's randomness and a birthday into a
//! 16-word polyseed, the keys and primary address it stands for, and the same
//! wallet as an older 25-word phrase (for apps that can't read polyseed, like
//! the Monero GUI). Only the private view key and the public spend key ever
//! leave the page: monokulo registers those, watch-only. The phrase and the
//! private spend key stay in the page's memory.
//!
//! [`qr_svg`] draws a QR code, so a wallet app can scan the phrase straight
//! off the screen.

use curve25519_dalek::edwards::EdwardsPoint;
use curve25519_dalek::scalar::Scalar;
use sha3::{Digest, Keccak256};
use zeroize::Zeroizing;

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
mod wasm;

/// Which Monero network an address is for: only the address prefix differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Network {
    Mainnet,
    Stagenet,
    Testnet,
}

impl Network {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "mainnet" => Some(Self::Mainnet),
            "stagenet" => Some(Self::Stagenet),
            "testnet" => Some(Self::Testnet),
            _ => None,
        }
    }

    /// The standard address prefix (`cryptonote_config.h`).
    fn standard_prefix(self) -> u8 {
        match self {
            Self::Mainnet => 18,
            Self::Stagenet => 24,
            Self::Testnet => 53,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// The entropy didn't make a valid polyseed (it can't, once masked:
    /// kept for the library's own checks).
    Polyseed,
    /// The QR code couldn't hold the text.
    QrTooLong,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Polyseed => "the recovery phrase could not be made",
            Self::QrTooLong => "too much text for a QR code",
        })
    }
}

/// A freshly made wallet.
pub struct NewWallet {
    /// The 16-word polyseed (English), words separated by single spaces.
    pub phrase: Zeroizing<String>,
    /// The same wallet as a 25-word phrase.
    pub legacy_phrase: Zeroizing<String>,
    /// The polyseed's birthday as stored in it (unix seconds, rounded down
    /// to the polyseed's granularity).
    pub birthday: u64,
    pub address: String,
    pub view_key_hex: Zeroizing<String>,
    pub spend_pubkey_hex: String,
}

/// Makes a wallet from 32 bytes of randomness and its birthday (unix
/// seconds). Only the first 150 bits of `entropy` are used, as polyseed
/// specifies.
pub fn generate(entropy: [u8; 32], birthday: u64, network: Network) -> Result<NewWallet, Error> {
    let mut entropy = Zeroizing::new(entropy);
    // Polyseed's 150 secret bits: the rest must be clear (`Polyseed::new`).
    entropy[19..].fill(0);
    entropy[18] &= 0x3f;
    let seed = polyseed::Polyseed::from(polyseed::Language::English, 0, birthday, entropy)
        .map_err(|_| Error::Polyseed)?;
    let spend = Zeroizing::new(Scalar::from_bytes_mod_order(*seed.key()));
    let legacy = monero_seed::Seed::from_entropy(
        monero_seed::Language::English,
        Zeroizing::new(spend.to_bytes()),
    )
    .ok_or(Error::Polyseed)?;
    let view = Zeroizing::new(view_key_for(&spend));
    let spend_pub = EdwardsPoint::mul_base(&spend).compress().to_bytes();
    let view_pub = EdwardsPoint::mul_base(&view).compress().to_bytes();
    Ok(NewWallet {
        phrase: seed.to_string(),
        legacy_phrase: legacy.to_string(),
        birthday: seed.birthday(),
        address: standard_address(network, &spend_pub, &view_pub),
        view_key_hex: Zeroizing::new(hex::encode(view.to_bytes())),
        spend_pubkey_hex: hex::encode(spend_pub),
    })
}

/// Monero's private view key for a private spend key: Keccak-256 of the
/// spend key, reduced.
fn view_key_for(spend: &Scalar) -> Scalar {
    let digest = Keccak256::digest(spend.to_bytes());
    let mut bytes = Zeroizing::new([0u8; 32]);
    bytes.copy_from_slice(&digest);
    Scalar::from_bytes_mod_order(*bytes)
}

/// A standard address: prefix, public spend key, public view key and a
/// 4-byte Keccak checksum, in Monero's base58.
fn standard_address(network: Network, spend_pub: &[u8; 32], view_pub: &[u8; 32]) -> String {
    let mut data = Vec::with_capacity(65);
    data.push(network.standard_prefix());
    data.extend_from_slice(spend_pub);
    data.extend_from_slice(view_pub);
    base58_monero::encode_check(&data).expect("a 65-byte address always encodes")
}

/// `text` as a QR code: an SVG drawing, dark modules on light, with no
/// colours of its own (`currentColor`-free, the page styles its frame).
pub fn qr_svg(text: &str) -> Result<String, Error> {
    let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M)
        .map_err(|_| Error::QrTooLong)?;
    Ok(code
        .render::<qrcode::render::svg::Color<'_>>()
        .min_dimensions(240, 240)
        .quiet_zone(true)
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIRTHDAY: u64 = 1_791_400_000; // 2026-10-07

    fn sample(seed_byte: u8) -> NewWallet {
        generate([seed_byte; 32], BIRTHDAY, Network::Mainnet).unwrap()
    }

    #[test]
    fn the_birthday_is_kept_in_the_phrase() {
        let wallet = sample(3);
        assert!(wallet.birthday <= BIRTHDAY && BIRTHDAY - wallet.birthday < 60 * 60 * 24 * 31);
    }

    #[test]
    fn each_network_gets_its_own_address_prefix() {
        let prefixes: Vec<char> = [Network::Mainnet, Network::Stagenet, Network::Testnet]
            .into_iter()
            .map(|n| {
                generate([5; 32], BIRTHDAY, n)
                    .unwrap()
                    .address
                    .chars()
                    .next()
                    .unwrap()
            })
            .collect();
        assert_eq!(prefixes, vec!['4', '5', '9']);
    }

    #[test]
    fn different_entropy_makes_a_different_wallet_and_only_150_bits_count() {
        assert_ne!(sample(1).address, sample(2).address);
        let mut noisy = [4u8; 32];
        noisy[19..].fill(0xaa);
        noisy[18] |= 0xc0;
        assert_eq!(
            generate(noisy, BIRTHDAY, Network::Mainnet).unwrap().address,
            sample(4).address
        );
    }

    #[test]
    fn a_restore_link_fits_in_a_qr_code() {
        let wallet = sample(6);
        let link = format!(
            "monero_wallet:{}?seed={}&height=3400000&label=Copper%20Heron",
            wallet.address,
            wallet.phrase.replace(' ', "%20")
        );
        let svg = qr_svg(&link).unwrap();
        assert!(svg.starts_with("<?xml") || svg.starts_with("<svg"));
        assert!(qr_svg(&"x".repeat(5000)).is_err());
    }
}
