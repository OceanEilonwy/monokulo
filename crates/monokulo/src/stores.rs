//! A store's name and site, as the merchant types them (migration 0034).
//!
//! A store's site is a host, not a page. Browsers send only the origin
//! (scheme, host and port) in `Origin`, and trim a cross-site `Referer` to
//! it by default, so a path could never be checked; and everything
//! monokulo enforces already works per host: CORS, `frame-ancestors` and
//! verified domains, which cover the domain's subdomains too. So a pasted
//! URL keeps its host (and a port that isn't the scheme's own), and any
//! page on that host works.

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

/// What the merchant typed as the store's site, as the host it's kept as:
/// `https://Shop.Example/checkout?x=1` and `shop.example` are both
/// `shop.example`; `localhost:8080` keeps its port. `Err` says why it
/// can't be a site, for the field.
pub fn normalize_site(input: &str) -> Result<String, &'static str> {
    let input = input.trim();
    if input.is_empty() {
        return Err("Enter your site's address, like shop.example.");
    }
    let with_scheme = if input.contains("://") {
        input.to_owned()
    } else {
        format!("https://{input}")
    };
    let url = url::Url::parse(&with_scheme)
        .map_err(|_| "That isn't a web address. Enter it like shop.example.")?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(
            "Enter a web address, starting with https:// or just the name, like shop.example.",
        );
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Enter the address without a login in it, like shop.example.");
    }
    let host = url
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or("Enter your site's address, like shop.example.")?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host.is_empty() {
        return Err("Enter your site's address, like shop.example.");
    }
    Ok(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

/// A link to the site: `https://` unless it's this machine or an onion
/// service, which are served over plain http.
pub fn site_link(site: &str) -> String {
    let host = site.rsplit_once(':').map_or(site, |(host, _)| host);
    let plain =
        host == "localhost" || host == "127.0.0.1" || host == "[::1]" || host.ends_with(".onion");
    format!("{}://{site}", if plain { "http" } else { "https" })
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
            assert_eq!(normalize_site(typed).unwrap(), "shop.example", "{typed}");
        }
    }

    #[test]
    fn a_port_stays_only_when_it_isnt_the_schemes() {
        assert_eq!(normalize_site("localhost:8080").unwrap(), "localhost:8080");
        assert_eq!(
            normalize_site("https://shop.example:443/x").unwrap(),
            "shop.example"
        );
        assert_eq!(
            normalize_site("http://127.0.0.1:3000").unwrap(),
            "127.0.0.1:3000"
        );
    }

    #[test]
    fn what_cant_be_a_site_says_why() {
        assert!(normalize_site("").is_err());
        assert!(normalize_site("   ").is_err());
        assert!(normalize_site("ftp://shop.example").is_err());
        assert!(normalize_site("https://me:secret@shop.example").is_err());
        assert!(normalize_site("http://").is_err());
    }

    #[test]
    fn a_name_is_tidied_and_must_be_there() {
        assert_eq!(clean_name("  Corner   shop ").unwrap(), "Corner shop");
        assert!(clean_name("  ").is_err());
        assert!(clean_name(&"x".repeat(MAX_NAME_LEN + 1)).is_err());
    }

    #[test]
    fn local_and_onion_sites_link_over_plain_http() {
        assert_eq!(site_link("shop.example"), "https://shop.example");
        assert_eq!(site_link("localhost:8080"), "http://localhost:8080");
        assert_eq!(site_link("abc.onion"), "http://abc.onion");
    }
}
