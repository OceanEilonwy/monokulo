//! Onion addresses: what a v3 `.onion` name is, checked the way Tor checks
//! it (rend-spec, "Encoding onion addresses"), so a typo is caught when it
//! is typed rather than when nobody can reach it.

use sha3::{Digest, Sha3_256};

/// The 56 base32 characters before `.onion`.
const V3_LABEL_LEN: usize = 56;

/// `host` as a v3 onion name (lowercase, with any subdomains before it
/// kept), or why it isn't one. `shop.abc…xyz.onion` is the service
/// `abc…xyz.onion` too: subdomains of an onion address are the same
/// service.
pub fn v3_host(host: &str) -> Result<String, String> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let Some(name) = host.strip_suffix(".onion") else {
        return Err(format!("{host} is not an .onion address."));
    };
    let label = name.rsplit('.').next().unwrap_or(name);
    if label.len() != V3_LABEL_LEN {
        return Err(format!(
            "{host} is not a v3 onion address: those have {V3_LABEL_LEN} letters and digits before .onion."
        ));
    }
    let bytes = base32_decode(label).ok_or_else(|| {
        format!("{host} is not a v3 onion address: it has characters onion addresses don't use.")
    })?;
    // PUBKEY (32) | CHECKSUM (2) | VERSION (1).
    let (pubkey, rest) = bytes.split_at(32);
    let (checksum, version) = rest.split_at(2);
    if version != [3] {
        return Err(format!("{host} is not a v3 onion address."));
    }
    let mut hasher = Sha3_256::new();
    hasher.update(b".onion checksum");
    hasher.update(pubkey);
    hasher.update([3]);
    if hasher.finalize()[..2] != *checksum {
        return Err(format!(
            "{host} is not a valid onion address: its checksum is wrong, so a character is mistyped."
        ));
    }
    Ok(host)
}

/// The service an onion host belongs to: `abc…xyz.onion` for
/// `shop.abc…xyz.onion`. `host` must already be a [`v3_host`].
pub fn service_of(host: &str) -> &str {
    let without = host.strip_suffix(".onion").unwrap_or(host);
    let label_start = without.rfind('.').map_or(0, |dot| dot + 1);
    &host[label_start..]
}

/// Whether `host` ends in `.onion` (valid or not).
pub fn is_onion_host(host: &str) -> bool {
    host.trim_end_matches('.')
        .to_ascii_lowercase()
        .ends_with(".onion")
}

/// Checks an `onion_uri` value: `http://<v3 address>.onion`, with nothing
/// after it but an optional `/`. Returns it without the trailing `/`.
/// Onion services are reached over plain `http`: Tor itself encrypts and
/// authenticates the connection end to end.
pub fn validate_onion_uri(value: &str) -> Result<String, String> {
    let value = value.trim();
    let problem = "Enter this instance's onion address, like http://abc…xyz.onion (56 letters and digits before .onion), with no path after it.";
    let parsed = url::Url::parse(value).map_err(|_| problem.to_string())?;
    let shape = parsed.scheme() == "http"
        && parsed.port().is_none()
        && parsed.path() == "/"
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && parsed.username().is_empty()
        && parsed.password().is_none();
    let host = parsed.host_str().ok_or_else(|| problem.to_string())?;
    if !shape || !is_onion_host(host) {
        return Err(problem.to_string());
    }
    let host = v3_host(host)?;
    if service_of(&host) != host {
        return Err(format!(
            "{problem} Leave out {}.",
            host.trim_end_matches(service_of(&host))
        ));
    }
    Ok(format!("http://{host}"))
}

/// RFC 4648 base32, lowercase, no padding: `None` for any other character
/// or a length that isn't whole bytes.
fn base32_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for c in text.bytes() {
        let value = match c {
            b'a'..=b'z' => c - b'a',
            b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        };
        buffer = (buffer << 5) | u32::from(value);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    (bits < 5 && buffer == 0).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Tor Project's own onion service (torproject.org's Onion-Location).
    const TORPROJECT: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";

    #[test]
    fn a_real_v3_address_is_accepted_lowercased_with_its_subdomains() {
        assert_eq!(v3_host(TORPROJECT).unwrap(), TORPROJECT);
        assert_eq!(v3_host(&TORPROJECT.to_uppercase()).unwrap(), TORPROJECT);
        let shop = format!("shop.{TORPROJECT}");
        assert_eq!(v3_host(&shop).unwrap(), shop);
        assert_eq!(service_of(&shop), TORPROJECT);
        assert_eq!(service_of(TORPROJECT), TORPROJECT);
    }

    #[test]
    fn a_mistyped_short_or_old_address_is_refused_saying_why() {
        let mistyped = TORPROJECT.replacen("2gzy", "2gzz", 1);
        assert!(v3_host(&mistyped).unwrap_err().contains("checksum"));
        // A v2 address: 16 characters.
        assert!(v3_host("expyuzz4wqqyqhjn.onion")
            .unwrap_err()
            .contains("56"));
        assert!(v3_host("shop.example")
            .unwrap_err()
            .contains("not an .onion"));
        let bad_char = TORPROJECT.replacen('2', "1", 1);
        assert!(v3_host(&bad_char).unwrap_err().contains("characters"));
    }

    #[test]
    fn an_onion_uri_is_plain_http_to_the_service_itself() {
        assert_eq!(
            validate_onion_uri(&format!(" http://{TORPROJECT}/ ")).unwrap(),
            format!("http://{TORPROJECT}")
        );
        for bad in [
            format!("https://{TORPROJECT}"),
            format!("http://{TORPROJECT}:8080"),
            format!("http://{TORPROJECT}/pay"),
            format!("http://shop.{TORPROJECT}"),
            "http://pay.example.com".to_string(),
            TORPROJECT.to_string(),
        ] {
            assert!(validate_onion_uri(&bad).is_err(), "{bad} should be refused");
        }
    }
}
