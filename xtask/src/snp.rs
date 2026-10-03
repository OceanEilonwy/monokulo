//! `cargo xtask snp-id-key` and `cargo xtask snp-id-block`: the engine
//! image's ID key, and the ID block it signs (`snp_attest::id_block`).
//!
//! The ID key's digest is what key custody trusts an engine image by: built
//! into `key-custody` (`crates/key-custody/src/official_id_key_digest.txt`),
//! and so into key-custody-cli and monokulo's key entry. The private key is
//! kept as a CI secret, `SNP_ID_KEY` (96 hex characters), and used only by
//! the `snp-id-block` workflow to sign an engine image's ID block.

use std::{fs, path::Path};

use p384::ecdsa::SigningKey;
use snp_attest::id_block::{self, IdBlock};

/// Where the official digest is built from.
const DIGEST_FILE: &str = "crates/key-custody/src/official_id_key_digest.txt";

fn signing_key_from_env() -> Result<SigningKey, String> {
    let hex_key = std::env::var("SNP_ID_KEY")
        .map_err(|_| "set SNP_ID_KEY to the ID key (96 hex characters)".to_owned())?;
    let bytes = hex::decode(hex_key.trim()).map_err(|_| "SNP_ID_KEY is not hex".to_owned())?;
    SigningKey::from_slice(&bytes).map_err(|_| "SNP_ID_KEY is not a P-384 private key".to_owned())
}

fn new_signing_key() -> Result<SigningKey, String> {
    loop {
        let mut bytes = [0u8; 48];
        getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
        if let Ok(key) = SigningKey::from_slice(&bytes) {
            return Ok(key);
        }
    }
}

/// `snp-id-key`: makes a new ID key, prints it (for the CI secret) and
/// writes its digest into `key-custody`. With `--from-env`, writes the
/// digest of the key in `SNP_ID_KEY` instead.
pub fn id_key(root: &Path, from_env: bool) -> Result<bool, String> {
    let key = if from_env {
        signing_key_from_env()?
    } else {
        new_signing_key()?
    };
    let digest = hex::encode(id_block::id_key_digest(key.verifying_key()));
    fs::write(root.join(DIGEST_FILE), format!("{digest}\n")).map_err(|e| e.to_string())?;
    eprintln!("ID key digest {digest}, written to {DIGEST_FILE}.");
    if !from_env {
        eprintln!(
            "The private key is below. Store it as the repository secret SNP_ID_KEY \
             (gh secret set SNP_ID_KEY) and nowhere else, then commit {DIGEST_FILE}."
        );
        println!("{}", hex::encode(key.to_bytes()));
    }
    Ok(true)
}

/// 16 bytes from hex, or `default` (text, zero-padded) when not given.
fn sixteen(value: Option<&str>, default: &str) -> Result<[u8; 16], String> {
    match value {
        Some(text) => hex::decode(text)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| format!("{text:?} is not 32 hex characters")),
        None => {
            let mut out = [0u8; 16];
            out[..default.len()].copy_from_slice(default.as_bytes());
            Ok(out)
        }
    }
}

/// Standard base64 with padding, as QEMU's `id-block=`/`id-auth=` take.
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(TABLE[(n >> (18 - 6 * i) & 0x3F) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// `snp-id-block`: signs an ID block for an engine image with the key in
/// `SNP_ID_KEY`, and writes it and its auth info (raw and base64) to `out`.
pub fn id_block(args: &[&str]) -> Result<bool, String> {
    let mut measurement = None;
    let mut guest_svn = None;
    let mut family_id = None;
    let mut image_id = None;
    let mut policy = "30000".to_owned();
    let mut out = None;
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let value = rest
            .next()
            .ok_or_else(|| format!("{flag} needs a value"))?
            .to_string();
        match *flag {
            "--measurement" => measurement = Some(value),
            "--guest-svn" => guest_svn = Some(value),
            "--family-id" => family_id = Some(value),
            "--image-id" => image_id = Some(value),
            "--policy" => policy = value,
            "--out" => out = Some(value),
            other => return Err(format!("unknown option {other}")),
        }
    }
    let measurement: [u8; 48] = hex::decode(measurement.ok_or("--measurement is required")?)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or("--measurement is the image's launch measurement, 96 hex characters")?;
    let guest_svn: u32 = guest_svn
        .ok_or("--guest-svn is required")?
        .parse()
        .map_err(|_| "--guest-svn is a number")?;
    let policy = u64::from_str_radix(policy.trim_start_matches("0x"), 16)
        .map_err(|_| "--policy is the guest policy, in hex")?;
    let out = out.ok_or("--out is required")?;
    let block = IdBlock {
        measurement,
        family_id: sixteen(family_id.as_deref(), "monokulo")?,
        image_id: sixteen(image_id.as_deref(), "engine")?,
        guest_svn,
        policy,
    };
    let key = signing_key_from_env()?;
    let auth = id_block::sign(&block, &key);
    let out = Path::new(&out);
    fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let block_bytes = block.to_bytes();
    for (name, bytes) in [("id-block", &block_bytes[..]), ("id-auth", &auth[..])] {
        fs::write(out.join(format!("{name}.bin")), bytes).map_err(|e| e.to_string())?;
        fs::write(out.join(format!("{name}.b64")), base64(bytes)).map_err(|e| e.to_string())?;
    }
    eprintln!(
        "Signed the ID block for image {} (security version {guest_svn}) with ID key {}; wrote {}.",
        hex::encode(measurement),
        hex::encode(id_block::id_key_digest(key.verifying_key())),
        out.display()
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xFF, 0xEE]), "/+4=");
    }

    #[test]
    fn ids_are_hex_or_a_padded_name() {
        assert_eq!(&sixteen(None, "engine").unwrap()[..7], b"engine\0");
        assert_eq!(sixteen(Some(&"ab".repeat(16)), "x").unwrap(), [0xAB; 16]);
        assert!(sixteen(Some("ab"), "x").is_err());
    }
}
