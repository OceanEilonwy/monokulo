//! A store's name and site, as the merchant types them (migration 0034),
//! and what a payment is for: an order's description, or the store's
//! payment description in its place (migration 0041).
//!
//! A store's site is a host, not a page. Browsers send only the origin
//! (scheme, host and port) in `Origin`, and trim a cross-site `Referer` to
//! it by default, so a path could never be checked; and everything
//! monokulo enforces already works per host: CORS, `frame-ancestors` and
//! verified domains, which cover the domain's subdomains too. So a pasted
//! URL keeps its host (and a port that isn't the scheme's own), and any
//! page on that host works.

use std::fmt;

/// The longest name a store may have.
pub const MAX_NAME_LEN: usize = 60;

/// A store name as typed: trimmed, inner runs of spaces made one. `Err`
/// says what's wrong with it, for the field.
pub fn clean_name(raw: &str) -> Result<String, &'static str> {
    let name = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return Err("Give the store a name.");
    }
    if name.chars().count() > MAX_NAME_LEN {
        return Err("A store name can be at most 60 characters.");
    }
    Ok(name)
}

/// The longest description an order, or a store's payment description,
/// may have, in characters.
pub const MAX_DESCRIPTION_CHARS: usize = 140;

/// Why a description was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescriptionProblem {
    TooLong,
    /// It holds a control character: a line break, a tab, or another that
    /// would show as nothing or break the line the payer reads it on.
    NotOneLine,
}

impl DescriptionProblem {
    /// What a description must be, to follow "must be": "at most 140
    /// characters".
    pub fn requirement(self) -> &'static str {
        match self {
            DescriptionProblem::TooLong => "at most 140 characters",
            DescriptionProblem::NotOneLine => {
                "one line of plain text, without line breaks or other control characters"
            }
        }
    }
}

/// What a payment is for, as typed: trimmed, `None` when that leaves
/// nothing, and refused when it's longer than [`MAX_DESCRIPTION_CHARS`]
/// (counted in Unicode scalar values) or isn't one line of plain text.
/// The payer reads it on the checkout, so it's shown as typed, escaped.
pub fn clean_description(raw: &str) -> Result<Option<String>, DescriptionProblem> {
    let description = raw.trim();
    if description.is_empty() {
        return Ok(None);
    }
    if description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(DescriptionProblem::TooLong);
    }
    if description.chars().any(char::is_control) {
        return Err(DescriptionProblem::NotOneLine);
    }
    Ok(Some(description.to_owned()))
}

/// A store's site: a host, lowercase, with a port only when it isn't the
/// scheme's own (`shop.example`, `localhost:8080`). Made by
/// [`Site::parse`] from what someone typed or sent, so two spellings of
/// one site are one value, and a site can't be mistaken for any other
/// string. A store without a site has `None`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Site(String);

/// Why something typed can't be a site, said for the field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidSite(&'static str);

impl fmt::Display for InvalidSite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl Site {
    /// What the merchant typed as the store's site, as the host it's kept
    /// as: `https://Shop.Example/checkout?x=1` and `shop.example` are both
    /// `shop.example`; `localhost:8080` keeps its port.
    pub fn parse(input: &str) -> Result<Self, InvalidSite> {
        let input = input.trim();
        let missing = InvalidSite("Enter your site's address, like shop.example.");
        if input.is_empty() {
            return Err(missing);
        }
        let with_scheme = if input.contains("://") {
            input.to_owned()
        } else {
            format!("https://{input}")
        };
        let url = url::Url::parse(&with_scheme)
            .map_err(|_| InvalidSite("That isn't a web address. Enter it like shop.example."))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(InvalidSite(
                "Enter a web address, starting with https:// or just the name, like shop.example.",
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(InvalidSite(
                "Enter the address without a login in it, like shop.example.",
            ));
        }
        let host = url
            .host_str()
            .ok_or(missing)?
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if host.is_empty() {
            return Err(missing);
        }
        Ok(Self(match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host,
        }))
    }

    /// A site as monokulo stored it, which it parsed before storing: `''`
    /// is none.
    pub(crate) fn stored(site: &str) -> Option<Self> {
        (!site.is_empty()).then(|| Self(site.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A link to the site: `https://` unless it's this machine or an onion
    /// service, which are served over plain http.
    pub fn link(&self) -> String {
        let site = self.as_str();
        let host = site.rsplit_once(':').map_or(site, |(host, _)| host);
        let plain = host == "localhost"
            || host == "127.0.0.1"
            || host == "[::1]"
            || host.ends_with(".onion");
        format!("{}://{site}", if plain { "http" } else { "https" })
    }
}

impl fmt::Display for Site {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A store's site as it's stored: `''` for none.
pub fn stored_site(site: Option<&Site>) -> &str {
    site.map_or("", Site::as_str)
}

/// Records the hash of each store's secret key that doesn't have one yet
/// (migration 0044), so the API can find the store by its key. Run once at
/// start; a store whose key can't be decrypted is logged and left without,
/// and can't use the API with its key until it can. Returns how many it
/// filled.
pub fn fill_secret_key_hashes(db: &crate::db::Db, key: &crate::crypto::AtRestKey) -> usize {
    let missing = match db.stores_missing_secret_key_hash() {
        Ok(missing) => missing,
        Err(e) => {
            tracing::error!(error = %e, "stores' secret key hashes could not be read");
            return 0;
        }
    };
    let mut filled = 0;
    for (id, encrypted) in missing {
        let hash = match crate::crypto::decrypt(
            key,
            crate::crypto::Binding::StoreSecret(id.as_str()),
            &encrypted,
        ) {
            Ok(secret) => shared::auth::RawToken::presented(&secret).hash(),
            Err(e) => {
                tracing::error!(store.id = %id, error = %e, "a store's secret key could not be decrypted to record its hash");
                continue;
            }
        };
        match db.set_secret_key_hash(&id, &hash) {
            Ok(()) => filled += 1,
            Err(e) => {
                tracing::error!(store.id = %id, error = %e, "a store's secret key hash could not be recorded")
            }
        }
    }
    filled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pasted_address_keeps_only_its_host() {
        for typed in [
            "shop.example",
            "https://shop.example",
            "https://Shop.Example/checkout?step=2#pay",
            "http://shop.example/",
            "  SHOP.example.  ",
        ] {
            assert_eq!(
                Site::parse(typed).unwrap().as_str(),
                "shop.example",
                "{typed}"
            );
        }
    }

    #[test]
    fn a_port_stays_only_when_it_isnt_the_schemes() {
        assert_eq!(
            Site::parse("localhost:8080").unwrap().as_str(),
            "localhost:8080"
        );
        assert_eq!(
            Site::parse("https://shop.example:443/x").unwrap().as_str(),
            "shop.example"
        );
        assert_eq!(
            Site::parse("http://127.0.0.1:3000").unwrap().as_str(),
            "127.0.0.1:3000"
        );
    }

    #[test]
    fn what_cant_be_a_site_says_why() {
        assert!(Site::parse("").is_err());
        assert!(Site::parse("   ").is_err());
        assert!(Site::parse("ftp://shop.example").is_err());
        assert!(Site::parse("https://me:secret@shop.example").is_err());
        assert!(Site::parse("http://").is_err());
    }

    #[test]
    fn a_description_is_one_trimmed_line_of_up_to_140_characters() {
        assert_eq!(
            clean_description("  Flat white, 2× croissant ").unwrap(),
            Some("Flat white, 2× croissant".to_owned())
        );
        assert_eq!(clean_description("   ").unwrap(), None);
        // Counted in characters, not bytes.
        assert!(clean_description(&"é".repeat(MAX_DESCRIPTION_CHARS)).is_ok());
        assert_eq!(
            clean_description(&"x".repeat(MAX_DESCRIPTION_CHARS + 1)),
            Err(DescriptionProblem::TooLong)
        );
        for broken in ["two\nlines", "a\ttab", "a\u{7}bell", "a\u{85}next line"] {
            assert_eq!(
                clean_description(broken),
                Err(DescriptionProblem::NotOneLine),
                "{broken:?}"
            );
        }
    }

    #[test]
    fn a_name_is_tidied_and_must_be_there() {
        assert_eq!(clean_name("  Corner   shop ").unwrap(), "Corner shop");
        assert!(clean_name("  ").is_err());
        assert!(clean_name(&"x".repeat(MAX_NAME_LEN + 1)).is_err());
    }

    #[test]
    fn local_and_onion_sites_link_over_plain_http() {
        assert_eq!(
            Site::parse("shop.example").unwrap().link(),
            "https://shop.example"
        );
        assert_eq!(
            Site::parse("localhost:8080").unwrap().link(),
            "http://localhost:8080"
        );
        assert_eq!(Site::parse("abc.onion").unwrap().link(), "http://abc.onion");
    }
}
