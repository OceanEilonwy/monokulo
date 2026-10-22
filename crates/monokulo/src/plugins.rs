//! Shop plugins, as they describe themselves (migration 0042). monokulo
//! keeps no list of plugins: any plugin that speaks the plugin API
//! (`docs/plugins.md` on the Pages site) connects through
//! `/plugins/connect/{plugin}` under a slug of its own choosing, and may give a
//! name to show for it. The shop's host, which the connect flow checks, is
//! what identifies a plugin to the merchant; the name is only a label.
//! Connected apps (`crate::apps`) name themselves by the same rules.
//!
//! What a plugin sends is parsed once, where it arrives, into a
//! [`PluginSlug`] and a [`PluginName`]; past that, code holds a [`Plugin`],
//! so a slug and a name can't be swapped or shown unchecked.

use std::fmt;

/// The longest name a plugin may give itself, in characters.
pub const MAX_NAME_LEN: usize = 40;

/// Where monokulo's own orders and reports come from (`http::pay`'s
/// `order_source`, the logs route): no plugin may take one of these as its
/// slug, or its orders would read as the till's or the website's.
const RESERVED_SLUGS: [&str; 5] = ["api", "dashboard", "plugin", "pos", "website"];

/// The name a plugin goes by in URLs, `Monokulo-Client` and the store's
/// records: a lowercase ASCII letter, then 1 to 31 lowercase letters,
/// digits or hyphens (`woocommerce`, `prestashop`), and not one of
/// monokulo's own sources.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginSlug(String);

impl PluginSlug {
    /// `raw` as a slug, or `None` when it can't be one.
    pub fn parse(raw: &str) -> Option<Self> {
        let mut chars = raw.chars();
        let valid = chars.next().is_some_and(|c| c.is_ascii_lowercase())
            && (2..=32).contains(&raw.len())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && !RESERVED_SLUGS.contains(&raw);
        valid.then(|| Self(raw.to_owned()))
    }

    /// A slug monokulo stored itself (an integration's kind, an order's
    /// source), which it parsed before storing.
    pub(crate) fn stored(kind: &str) -> Self {
        Self(kind.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PluginSlug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a plugin calls itself ("PrestaShop"), made safe to keep and show:
/// one line, inner runs of spaces made one, no invisible formatting
/// characters, at most [`MAX_NAME_LEN`] characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginName(String);

impl PluginName {
    /// The name a plugin sent, cleaned; `None` when nothing is left.
    pub fn clean(raw: &str) -> Option<Self> {
        let name = raw
            .split(|c: char| c.is_whitespace() || c.is_control() || invisible(c))
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        (!name.is_empty()).then(|| Self(name.chars().take(MAX_NAME_LEN).collect()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Characters that change how the text around them reads without showing
/// themselves: bidirectional overrides and isolates, zero-width spaces and
/// joiners, the byte order mark. A plugin's name could otherwise reorder
/// the shop's host printed after it.
fn invisible(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{FEFF}')
}

/// A plugin: its slug, and the name it gave, if it gave one. Shown
/// (`Display`) by its name, else by its slug with a capital first letter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plugin {
    slug: PluginSlug,
    name: Option<PluginName>,
}

impl Plugin {
    pub fn new(slug: PluginSlug, name: Option<PluginName>) -> Self {
        Self { slug, name }
    }

    /// A plugin as it was sent, in a link or a form: `None` when `slug`
    /// can't name one. A `name` with nothing left once cleaned is none.
    pub fn parse(slug: &str, name: &str) -> Option<Self> {
        Some(Self::new(PluginSlug::parse(slug)?, PluginName::clean(name)))
    }

    /// A plugin as monokulo stored it (`store_integrations`), with `''`
    /// for no name.
    pub(crate) fn stored(kind: &str, name: &str) -> Self {
        Self::new(PluginSlug::stored(kind), PluginName::clean(name))
    }

    pub fn slug(&self) -> &PluginSlug {
        &self.slug
    }

    pub fn name(&self) -> Option<&PluginName> {
        self.name.as_ref()
    }
}

impl fmt::Display for Plugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(name) = &self.name {
            return f.write_str(name.as_str());
        }
        let mut chars = self.slug.as_str().chars();
        if let Some(first) = chars.next() {
            write!(f, "{}{}", first.to_uppercase(), chars.as_str())?;
        }
        Ok(())
    }
}

/// A plugin and the shop it's connecting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectingPlugin {
    pub plugin: Plugin,
    pub site: crate::stores::Site,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid(slug: &str) -> bool {
        PluginSlug::parse(slug).is_some()
    }

    #[test]
    fn slugs_are_lowercase_words() {
        for slug in ["woocommerce", "prestashop", "open-cart", "x1"] {
            assert!(valid(slug), "{slug}");
        }
        let too_long = "a".repeat(33);
        for slug in [
            "", "a", "1cart", "-cart", "Cart", "my_cart", "cart/x", "cärt", &too_long,
        ] {
            assert!(!valid(slug), "{slug}");
        }
        assert!(valid(&"a".repeat(32)));
        // monokulo's own sources are not plugins.
        for slug in RESERVED_SLUGS {
            assert!(!valid(slug), "{slug}");
        }
        assert!(valid("custom"));
    }

    fn clean(raw: &str) -> Option<String> {
        PluginName::clean(raw).map(|name| name.as_str().to_owned())
    }

    /// A name can't carry characters that reorder or hide the text after it.
    #[test]
    fn a_name_loses_its_invisible_formatting() {
        assert_eq!(
            clean("Acme\u{202E}moc.live").as_deref(),
            Some("Acme moc.live")
        );
        assert_eq!(
            clean("A\u{200B}c\u{2066}m\u{FEFF}e").as_deref(),
            Some("A c m e")
        );
        assert_eq!(clean("\u{202E}\u{2069}"), None);
    }

    #[test]
    fn a_name_is_one_trimmed_bounded_line() {
        assert_eq!(clean("  Presta\n\tShop  ").as_deref(), Some("Presta Shop"));
        assert_eq!(clean("Open\u{7}Cart").as_deref(), Some("Open Cart"));
        assert_eq!(clean(" \n "), None);
        assert_eq!(
            clean(&"é".repeat(50)).unwrap().chars().count(),
            MAX_NAME_LEN
        );
    }

    #[test]
    fn a_plugin_is_shown_by_its_name_else_its_slug() {
        let shown = |slug: &str, name: &str| Plugin::parse(slug, name).unwrap().to_string();
        assert_eq!(shown("prestashop", "PrestaShop"), "PrestaShop");
        assert_eq!(shown("opencart", ""), "Opencart");
        assert_eq!(shown("whmcs", "  "), "Whmcs");
        assert_eq!(Plugin::parse("Shop", "Shop"), None);
    }
}
