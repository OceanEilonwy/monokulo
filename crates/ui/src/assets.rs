//! The package's web side, for Rust consumers: the stylesheets, scripts,
//! fonts and icons in `web/`, which npm consumers import as `@monokulo/ui`.
//! A server bakes these into its binary and serves them itself; nothing is
//! read from disk at run time. The islands' script (`web/islands.ts`, the
//! interactive components' Solid behaviours) needs a bundler: a server
//! builds it with Vite (`vite.islands.config.js`), as monokulo's build
//! script does.

/// Every colour, spacing, radius and type token, light and dark: the only
/// place a colour is defined.
pub const TOKENS_CSS: &str = include_str!("../web/tokens.css");

/// Every component's stylesheet, in cascade order, painted with
/// [`TOKENS_CSS`]'s roles. The POS's artwork (`status.css`) is left out:
/// only the POS shows it.
pub const COMPONENTS_CSS: &str = concat!(
    include_str!("../web/components/primitives.css"),
    "\n",
    include_str!("../web/components/select.css"),
    "\n",
    include_str!("../web/components/content.css"),
    "\n",
    include_str!("../web/components/dialog.css"),
    "\n",
    include_str!("../web/components/forms.css"),
    "\n",
    include_str!("../web/components/sortable.css"),
    "\n",
    include_str!("../web/components/navigation.css"),
    "\n",
    include_str!("../web/components/steps.css"),
    "\n",
    include_str!("../web/components/action-card.css"),
    "\n",
    include_str!("../web/components/settings.css"),
    "\n",
    include_str!("../web/components/patterns.css"),
    "\n",
    include_str!("../web/components/monitoring.css"),
    "\n",
    include_str!("../web/components/chrome.css"),
);

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
}
