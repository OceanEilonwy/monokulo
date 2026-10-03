//! `key-custody-cli`: encrypts a store's watch-only keys for a monokulo
//! engine's SEV-SNP key storage, on the merchant's own computer.
//!
//! `key-custody-cli seal --bundle <address or file>` reads a bundle (from the
//! key entry form's link), checks it with `key-custody`'s transport (the
//! report is AMD-signed up to the pinned root, the guest can't be debugged,
//! its image is signed by the trusted ID key at the minimum security version
//! or later, and the bundle's key is the one the report vouches for), asks for
//! the view key and the spend public key, and prints them encrypted to that
//! engine, for pasting into the form.
//!
//! What it trusts is built in: AMD's roots (`snp-attest`) and the official
//! engine ID key (`key-custody`'s `official_id_key_digest.txt`), so a site
//! that served a doctored bundle or page can't change it. `--trust-id-key`
//! replaces the official key for an instance that runs its own signed engine
//! image, and says so loudly.

use key_custody::transport::{self, Anchor, Bundle, TcbFloor, TrustPolicy, Verified, KEYS_LEN};
use zeroize::Zeroizing;

/// This build's version: the monokulo release it belongs to.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Which engine images to trust: `--trust-id-key` if given (and `true`,
/// for the warning), else the official key built in; at `min_guest_svn` or
/// later, on firmware at `min_tcb` (`bootloader,tee,snp,microcode`) or
/// later.
pub fn trust_policy(
    trust_id_key: Option<&str>,
    min_guest_svn: u32,
    min_tcb: &str,
) -> Result<(TrustPolicy, bool), String> {
    let min_tcb = TcbFloor::parse(min_tcb).map_err(|e| format!("--min-tcb: {e}"))?;
    let (id_key_digest, custom) = match trust_id_key {
        Some(text) => (
            transport::parse_id_key_digest(text).map_err(|e| format!("--trust-id-key: {e}"))?,
            true,
        ),
        None => (
            transport::official_id_key_digest().ok_or(
                "this build of key-custody-cli has no official engine ID key built in: pass the digest the \
                 monokulo operator gives you with --trust-id-key",
            )?,
            false,
        ),
    };
    Ok((
        TrustPolicy {
            id_key_digest,
            min_guest_svn,
            min_tcb,
        },
        custom,
    ))
}

/// The bundle named by `source`: fetched with `fetch` when it is an http(s)
/// address, read from the file otherwise.
pub fn load_bundle(
    source: &str,
    fetch: impl FnOnce(&str) -> Result<String, String>,
) -> Result<Bundle, String> {
    let text = if source.starts_with("https://") || source.starts_with("http://") {
        fetch(source)?
    } else {
        std::fs::read_to_string(source).map_err(|e| format!("can't read {source}: {e}"))?
    };
    serde_json::from_str(&text).map_err(|e| {
        format!("{source} is not a key custody bundle ({e}); use the link from the key entry form")
    })
}

/// The keys as sealed, from their hex: the private view key, then the
/// public spend key.
pub fn parse_keys(
    view_key_hex: &str,
    spend_pubkey_hex: &str,
) -> Result<Zeroizing<[u8; KEYS_LEN]>, String> {
    let mut keys = Zeroizing::new([0u8; KEYS_LEN]);
    for (what, text, at) in [
        ("private view key", view_key_hex, 0),
        ("public spend key", spend_pubkey_hex, 32),
    ] {
        let bytes =
            Zeroizing::new(hex::decode(text.trim()).map_err(|_| format!("the {what} is not hex"))?);
        if bytes.len() != 32 {
            return Err(format!(
                "the {what} is 64 hex characters, not {}",
                text.trim().len()
            ));
        }
        keys[at..at + 32].copy_from_slice(&bytes);
    }
    Ok(keys)
}

/// Checks `bundle` under `policy` (against AMD's chain, at `now`) and
/// returns what to encrypt to.
pub fn check(bundle: &Bundle, policy: &TrustPolicy, now: i64) -> Result<Verified, String> {
    transport::verify_bundle(bundle, policy, &Anchor::Amd, now).map_err(|e| match e {
        transport::TransportError::Version { got } => format!(
            "this key-custody-cli ({VERSION}) reads bundle format {}, and this bundle is format {got}: download \
             the key-custody-cli release that matches the monokulo site you're using (the key entry form links it)",
            transport::PROTOCOL_VERSION
        ),
        other => other.to_string(),
    })
}

/// The keys, encrypted for the engine `verified` names: the text to paste.
pub fn seal(verified: &Verified, keys: &[u8; KEYS_LEN]) -> Result<String, String> {
    transport::seal(verified, keys)
        .map(|envelope| envelope.to_text())
        .map_err(|e| e.to_string())
}

/// What was checked, for the merchant to read before pasting.
pub fn describe(verified: &Verified, policy: &TrustPolicy, custom: bool) -> String {
    format!(
        "Checked: an AMD SEV-SNP engine, image {} at security version {}, signed by {} ID key {}.",
        hex::encode(verified.measurement),
        verified.guest_svn,
        if custom {
            "the --trust-id-key"
        } else {
            "the official"
        },
        hex::encode(policy.id_key_digest),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_two_32_byte_hex_values() {
        let keys = parse_keys(&"01".repeat(32), &"02".repeat(32)).unwrap();
        assert_eq!(keys[0], 1);
        assert_eq!(keys[63], 2);
        assert!(parse_keys("zz", &"02".repeat(32))
            .unwrap_err()
            .contains("not hex"));
        assert!(parse_keys(&"01".repeat(31), &"02".repeat(32))
            .unwrap_err()
            .contains("64 hex characters"));
    }

    #[test]
    fn a_trusted_id_key_given_replaces_the_official_one_and_is_flagged() {
        let (policy, custom) = trust_policy(Some(&"cd".repeat(48)), 4, "1,2,3,4").unwrap();
        assert!(custom);
        assert_eq!(policy.id_key_digest, [0xCD; 48]);
        assert_eq!(policy.min_guest_svn, 4);
        assert_eq!(policy.min_tcb.to_text(), "1,2,3,4");
        assert!(trust_policy(Some("cd"), 0, "")
            .unwrap_err()
            .starts_with("--trust-id-key"));
        assert!(trust_policy(Some(&"cd".repeat(48)), 0, "1,2")
            .unwrap_err()
            .starts_with("--min-tcb"));
        match transport::official_id_key_digest() {
            Some(official) => {
                assert_eq!(trust_policy(None, 0, "").unwrap().0.id_key_digest, official);
            }
            None => assert!(trust_policy(None, 0, "")
                .unwrap_err()
                .contains("--trust-id-key")),
        }
    }

    #[test]
    fn a_bundle_comes_from_its_address_or_a_file() {
        let fetched = load_bundle("https://pay.example.com/key-custody/bundles/x", |url| {
            assert!(url.ends_with("/bundles/x"));
            Err("offline".into())
        });
        assert_eq!(fetched.unwrap_err(), "offline");
        let missing = load_bundle("/nonexistent/key-custody-bundle.json", |_| unreachable!());
        assert!(missing.unwrap_err().contains("can't read"));
        let not_json = load_bundle("https://x", |_| Ok("<html>".into()));
        assert!(not_json.unwrap_err().contains("not a key custody bundle"));
    }

    #[test]
    fn a_bundle_of_another_format_names_the_cli_to_get() {
        let bundle = Bundle {
            v: transport::PROTOCOL_VERSION + 1,
            product: "Genoa".into(),
            report: String::new(),
            ask: String::new(),
            vcek: String::new(),
            crl: String::new(),
            public_key: String::new(),
            challenge: String::new(),
            action: transport::Action::Create,
            store: None,
            expires_at: i64::MAX,
        };
        let policy = TrustPolicy {
            id_key_digest: [0; 48],
            min_guest_svn: 0,
            min_tcb: TcbFloor::default(),
        };
        let error = check(&bundle, &policy, 0).unwrap_err();
        assert!(
            error.contains("download the key-custody-cli release that matches"),
            "{error}"
        );
    }
}
