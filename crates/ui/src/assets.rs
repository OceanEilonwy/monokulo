//! The package's web side, for Rust consumers: the stylesheets, scripts,
//! fonts and icons in `web/`, which npm consumers import as `@monokulo/ui`.
//! A server bakes these into its binary and serves them itself; nothing is
//! read from disk at run time. The islands' script (`web/islands.ts`, the
//! interactive components' Solid behaviours) needs a bundler: a server
//! builds it with Vite (`vite.islands.config.js`), as monokulo's build
//! script does.

/// Every colour, spacing, radius and type token, light and dark: the only
/// place a colour is defined. Baked with its compressed copies, for a
/// server to send ([`TOKENS_CSS`] is the text).
pub const TOKENS: static_assets::Baked = static_assets::bake!("web/tokens.css");
pub const TOKENS_CSS: &str = TOKENS.text();

/// Every component's stylesheet, in cascade order, painted with
/// [`TOKENS_CSS`]'s roles, baked with its compressed copies. The POS's
/// artwork (`status.css`) is left out: only the POS shows it.
pub const COMPONENTS: static_assets::Baked = static_assets::bake!(join "\n" [
    "web/components/primitives.css",
    "web/components/select.css",
    "web/components/content.css",
    "web/components/dialog.css",
    "web/components/forms.css",
    "web/components/sortable.css",
    "web/components/navigation.css",
    "web/components/steps.css",
    "web/components/action-card.css",
    "web/components/settings.css",
    "web/components/patterns.css",
    "web/components/monitoring.css",
    "web/components/chrome.css",
]);
pub const COMPONENTS_CSS: &str = COMPONENTS.text();

/// The UI typeface, Manrope, Latin subset, by weight.
pub const MANROPE_500: &[u8] = include_bytes!("../web/fonts/manrope-500.woff2");
pub const MANROPE_700: &[u8] = include_bytes!("../web/fonts/manrope-700.woff2");
pub const MANROPE_800: &[u8] = include_bytes!("../web/fonts/manrope-800.woff2");

/// The line icons (`web/icons`), as inline SVG that takes the text colour.
pub mod icons {
    pub const CHECK: &str = include_str!("../web/icons/check.svg").trim_ascii();
    pub const WARNING: &str = include_str!("../web/icons/warning.svg").trim_ascii();
    pub const INFO: &str = include_str!("../web/icons/info.svg").trim_ascii();
    pub const EYE: &str = include_str!("../web/icons/eye.svg").trim_ascii();
    pub const PLUS: &str = include_str!("../web/icons/plus.svg").trim_ascii();
    pub const MAIL: &str = include_str!("../web/icons/mail.svg").trim_ascii();
    pub const TRASH: &str = include_str!("../web/icons/trash.svg").trim_ascii();
    pub const STORE: &str = include_str!("../web/icons/store.svg").trim_ascii();
    pub const GLOBE: &str = include_str!("../web/icons/globe.svg").trim_ascii();
    pub const LOCK: &str = include_str!("../web/icons/lock.svg").trim_ascii();
    pub const LOCK_OPEN: &str = include_str!("../web/icons/lock-open.svg").trim_ascii();
    pub const EXPAND: &str = include_str!("../web/icons/expand.svg").trim_ascii();
    pub const REFRESH: &str = include_str!("../web/icons/refresh.svg").trim_ascii();
    pub const COPY: &str = include_str!("../web/icons/copy.svg").trim_ascii();
    pub const ARROW_LEFT: &str = include_str!("../web/icons/arrow-left.svg").trim_ascii();
    pub const ARROW_RIGHT: &str = include_str!("../web/icons/arrow-right.svg").trim_ascii();
    pub const CLOCK: &str = include_str!("../web/icons/clock.svg").trim_ascii();
    pub const SETTINGS: &str = include_str!("../web/icons/settings.svg").trim_ascii();
    pub const ORDER_NEW: &str = include_str!("../web/icons/order-new.svg").trim_ascii();
    pub const POS: &str = include_str!("../web/icons/pos.svg").trim_ascii();
    pub const SHARE: &str = include_str!("../web/icons/share.svg").trim_ascii();
    pub const WALLET: &str = include_str!("../web/icons/wallet.svg").trim_ascii();
    pub const EXTERNAL: &str = include_str!("../web/icons/external.svg").trim_ascii();
}
