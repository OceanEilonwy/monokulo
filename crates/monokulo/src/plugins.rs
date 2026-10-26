//! Shop plugins, as they describe themselves (migration 0042). monokulo
//! keeps no list of plugins: any plugin that speaks the plugin API
//! (docs.monokulo.com/plugins/) connects through
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

/// Where monokulo's own orders and reports come from (`api::v1::orders`'s
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
/// themselves: every format character (general category Cf) and every
/// Default_Ignorable_Code_Point, as Unicode 16 lists them. Among them are
/// bidirectional marks, overrides and isolates (the Arabic letter mark
/// too), zero-width spaces and joiners, the soft hyphen, variation
/// selectors, tag characters and the byte order mark. A plugin's name could
/// otherwise reorder the shop's host printed after it, or carry text two
/// names that look the same differ by.
fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{115F}'..='\u{1160}'
            | '\u{17B4}'..='\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF0}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E0FFF}'
    )
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

/// A plugin finishing its connection to a store: what [`connect`] records.
pub struct Connecting {
    pub store_id: crate::db::ConnectionId,
    pub slug: PluginSlug,
    /// The name it sent, cleaned. Without one (a plugin built before
    /// plugins named themselves) it keeps the name it had on this store
    /// before.
    pub name: Option<PluginName>,
    pub site: Option<crate::stores::Site>,
    pub version: String,
    /// Where it wants the store's events sent, already checked.
    pub webhook_url: Option<String>,
    pub at: i64,
}

/// What [`connect`] made.
pub struct Connected {
    /// The new webhook's signing secret, handed to the plugin once.
    pub webhook_signing_secret: Option<String>,
    /// The integration's id, or why it couldn't be recorded: the plugin has
    /// its key either way, and the store just doesn't show it connected.
    pub recorded: Result<String, crate::db::DbError>,
}

/// Connects a plugin to a store: its webhook is made, the webhook of the
/// plugin connected before goes (so the shop isn't sent each event twice),
/// and the connection is recorded, closing the earlier one. A store has
/// one plugin. Fails only when the webhook can't be made.
pub async fn connect(
    db: &crate::db::Database,
    key: &crate::crypto::AtRestKey,
    connecting: Connecting,
) -> Result<Connected, crate::webhooks::CreateError> {
    let Connecting {
        store_id,
        slug,
        name,
        site,
        version,
        webhook_url,
        at,
    } = connecting;
    let (webhook_id, webhook_signing_secret) = match &webhook_url {
        Some(url) => {
            let created =
                crate::webhooks::create(db, key, &store_id, url, &Default::default()).await?;
            (
                Some(created.webhook.id.to_string()),
                Some(created.signing_secret),
            )
        }
        None => (None, None),
    };

    let id = store_id.clone();
    let earlier = db
        .read(move |db| db.active_integration(&id))
        .await
        .ok()
        .flatten()
        .and_then(|i| i.webhook_id);
    if let Some(earlier) = earlier {
        let id = store_id.clone();
        let earlier = engine_protocol::ids::WebhookId::new(earlier);
        if let Err(e) = db.write(move |db| db.delete_webhook(&id, &earlier)).await {
            tracing::warn!(store.id = %store_id, error = %e, "the earlier connection's webhook could not be removed");
        }
    }

    let recorded = db
        .write(move |db| {
            let name = match name {
                Some(name) => Some(name),
                None => db.last_integration_name(&store_id, &slug)?,
            };
            db.connect_integration(&crate::db::NewStoreIntegration {
                store_id: &store_id,
                plugin: &Plugin::new(slug, name),
                site: site.as_ref(),
                version: &version,
                webhook_id: webhook_id.as_deref(),
                webhook_url: webhook_url.as_deref(),
                at,
            })
        })
        .await;
    Ok(Connected {
        webhook_signing_secret,
        recorded,
    })
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
        // The Arabic letter mark, a soft hyphen and a tag character are
        // format characters too.
        assert_eq!(
            clean("Acme\u{061C}\u{00AD}\u{E0041}Cart").as_deref(),
            Some("Acme Cart")
        );
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
