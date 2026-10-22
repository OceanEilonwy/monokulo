//! Shop plugins, as they describe themselves (migration 0042). monokulo
//! keeps no list of plugins: any plugin that speaks the plugin API
//! (`docs/plugins.md` on the Pages site) connects through
//! `/connect/{plugin}` under a slug of its own choosing, and may give a
//! name to show for it. The shop's host, which the connect flow checks, is
//! what identifies a plugin to the merchant; the name is only a label.

/// The longest name a plugin may give itself, in characters.
pub const MAX_NAME_LEN: usize = 40;

/// Whether `slug` can name a plugin: a lowercase ASCII letter, then 1 to
/// 31 lowercase letters, digits or hyphens (`woocommerce`, `prestashop`).
pub fn valid_slug(slug: &str) -> bool {
    let mut chars = slug.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && (2..=32).contains(&slug.len())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The name a plugin sent, made safe to keep and show: one line, inner
/// runs of spaces made one, at most [`MAX_NAME_LEN`] characters. `None`
/// when nothing is left.
pub fn clean_name(raw: &str) -> Option<String> {
    let name = raw
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    (!name.is_empty()).then(|| name.chars().take(MAX_NAME_LEN).collect())
}

/// What a plugin is called: the name it gave, else its slug with a
/// capital first letter.
pub fn display_name(slug: &str, name: &str) -> String {
    if let Some(name) = clean_name(name) {
        return name;
    }
    let mut chars = slug.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_lowercase_words() {
        for slug in ["woocommerce", "prestashop", "open-cart", "x1"] {
            assert!(valid_slug(slug), "{slug}");
        }
        let too_long = "a".repeat(33);
        for slug in [
            "", "a", "1cart", "-cart", "Cart", "my_cart", "cart/x", "cärt", &too_long,
        ] {
            assert!(!valid_slug(slug), "{slug}");
        }
        assert!(valid_slug(&"a".repeat(32)));
    }

    #[test]
    fn a_name_is_one_trimmed_bounded_line() {
        assert_eq!(
            clean_name("  Presta\n\tShop  ").as_deref(),
            Some("Presta Shop")
        );
        assert_eq!(clean_name("Open\u{7}Cart").as_deref(), Some("Open Cart"));
        assert_eq!(clean_name(" \n "), None);
        assert_eq!(
            clean_name(&"é".repeat(50)).unwrap().chars().count(),
            MAX_NAME_LEN
        );
    }

    #[test]
    fn the_display_name_falls_back_to_the_slug() {
        assert_eq!(display_name("prestashop", "PrestaShop"), "PrestaShop");
        assert_eq!(display_name("opencart", ""), "Opencart");
        assert_eq!(display_name("whmcs", "  "), "Whmcs");
    }
}
