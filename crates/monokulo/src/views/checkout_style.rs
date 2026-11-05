//! A store's checkout colours and shape: role colours for light and dark, a
//! transparent background, corners and a font from the customer's device. They
//! apply to every layout, and are written to a stylesheet the page links by the
//! hash of its content, since the page allows no inline style.
//!
//! Every value is a checked hex colour or one of a closed list, so what's
//! written here is trusted CSS, unlike a custom page's.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

/// One colour scheme's roles. `None` keeps the theme's own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Roles {
    pub accent: Option<String>,
    pub background: Option<String>,
    pub text: Option<String>,
    pub quiet: Option<String>,
    pub lines: Option<String>,
}

/// How round cards and buttons are.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Corners {
    Square,
    Small,
    #[default]
    Medium,
    Round,
}

impl Corners {
    pub const ALL: [(Corners, &'static str, &'static str); 4] = [
        (Corners::Square, "square", "Square"),
        (Corners::Small, "small", "Small"),
        (Corners::Medium, "medium", "Medium"),
        (Corners::Round, "round", "Round"),
    ];

    /// Its name in a form.
    pub fn name(self) -> &'static str {
        Corners::ALL
            .into_iter()
            .find(|(corners, ..)| *corners == self)
            .map_or("medium", |(_, name, _)| name)
    }

    pub fn from_name(name: &str) -> Option<Corners> {
        Corners::ALL
            .into_iter()
            .find(|(_, known, _)| *known == name)
            .map(|(corners, ..)| corners)
    }
}

/// A font from the customer's device, so nothing loads from elsewhere.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Font {
    #[default]
    Theme,
    Sans,
    Serif,
    Mono,
}

impl Font {
    pub const ALL: [(Font, &'static str, &'static str); 4] = [
        (Font::Theme, "theme", "Monokulo's own"),
        (Font::Sans, "sans", "System sans-serif"),
        (Font::Serif, "serif", "System serif"),
        (Font::Mono, "mono", "System monospace"),
    ];

    /// Its name in a form.
    pub fn name(self) -> &'static str {
        Font::ALL
            .into_iter()
            .find(|(font, ..)| *font == self)
            .map_or("theme", |(_, name, _)| name)
    }

    pub fn from_name(name: &str) -> Option<Font> {
        Font::ALL
            .into_iter()
            .find(|(_, known, _)| *known == name)
            .map(|(font, ..)| font)
    }
}

/// A store's checkout colours and shape.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CheckoutStyle {
    pub light: Roles,
    pub dark: Roles,
    pub transparent: bool,
    pub corners: Corners,
    pub font: Font,
}

/// A colour as typed: `#2f6b4f`, or empty for the theme's own. `None` for
/// anything else: it's written into a stylesheet, so nothing else may pass.
pub fn clean_colour(typed: &str) -> Option<Option<String>> {
    let typed = typed.trim().to_ascii_lowercase();
    if typed.is_empty() {
        return Some(None);
    }
    let digits = typed.strip_prefix('#')?;
    (digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_hexdigit())).then_some(Some(typed))
}

fn write_roles(css: &mut String, roles: &Roles) {
    let mut set = |tokens: &[&str], colour: &Option<String>| {
        // Checked again, whatever the stored form says: a restored or hand-edited
        // row mustn't reach a stylesheet as anything but a colour.
        if let Some(Some(colour)) = colour.as_deref().and_then(clean_colour) {
            for token in tokens {
                let _ = write!(css, "{token}:{colour};");
            }
        }
    };
    set(&["--accent", "--accent-text"], &roles.accent);
    set(&["--paper", "--paper-raised"], &roles.background);
    set(&["--ink"], &roles.text);
    set(&["--muted"], &roles.quiet);
    set(&["--line", "--line-strong"], &roles.lines);
    // What sits a shade off the background (the merchant's line, a button under
    // the pointer), so it stays readable on a background of the store's own.
    if roles.background.is_some() {
        css.push_str("--surface-sunken:color-mix(in srgb,var(--ink) 8%,var(--paper-raised));");
    }
}

impl CheckoutStyle {
    /// The stylesheet this style needs, if it changes anything.
    pub fn css(&self) -> Option<String> {
        if *self == CheckoutStyle::default() {
            return None;
        }
        let mut css = String::new();
        // Each scheme written as the theme itself picks it (`ui/web/tokens.css`):
        // pinned by `data-theme`, otherwise by the customer's device.
        for (roles, pinned, device, other) in [
            (
                &self.light,
                "light",
                "not (prefers-color-scheme:dark)",
                "dark",
            ),
            (&self.dark, "dark", "(prefers-color-scheme:dark)", "light"),
        ] {
            if *roles != Roles::default() {
                let mut body = String::new();
                write_roles(&mut body, roles);
                let _ = write!(
                    css,
                    ":root[data-theme=\"{pinned}\"]{{{body}}}@media {device}{{:root:not([data-theme=\"{other}\"]){{{body}}}}}"
                );
            }
        }
        let radius = match self.corners {
            Corners::Square => Some(["0", "0", "0"]),
            Corners::Small => Some(["2px", "3px", "5px"]),
            Corners::Medium => None,
            Corners::Round => Some(["10px", "16px", "24px"]),
        };
        let font = match self.font {
            Font::Theme => None,
            Font::Sans => Some("system-ui,-apple-system,\"Segoe UI\",sans-serif"),
            Font::Serif => Some("Georgia,\"Times New Roman\",serif"),
            Font::Mono => Some("ui-monospace,Menlo,Consolas,monospace"),
        };
        if radius.is_some() || font.is_some() {
            css.push_str(":root{");
            if let Some([sm, md, lg]) = radius {
                let _ = write!(css, "--radius-sm:{sm};--radius-md:{md};--radius-lg:{lg};");
            }
            if let Some(font) = font {
                let _ = write!(css, "--font-ui:{font};");
            }
            css.push('}');
        }
        if self.transparent {
            css.push_str("html,body{background:transparent}");
        }
        Some(css)
    }

    /// What's hard to read on its background, in which scheme: what a merchant is
    /// warned about, and may still save.
    pub fn contrast_warnings(&self) -> Vec<(&'static str, &'static str, f64)> {
        let mut warnings = Vec::new();
        for (scheme, roles) in [("light", &self.light), ("dark", &self.dark)] {
            for (name, colour) in [("Text", &roles.text), ("Quiet text", &roles.quiet)] {
                let ratio = colour
                    .as_deref()
                    .zip(roles.background.as_deref())
                    .and_then(|(colour, background)| contrast(colour, background));
                if let Some(ratio) = ratio.filter(|ratio| *ratio < 4.5) {
                    warnings.push((name, scheme, ratio));
                }
            }
        }
        warnings
    }
}

/// A stylesheet's name in its URL, from its content.
pub fn stylesheet_hash(css: &str) -> String {
    use sha2::Digest as _;
    hex::encode(&sha2::Sha256::digest(css.as_bytes())[..16])
}

fn luminance(colour: &str) -> Option<f64> {
    let digits = colour.strip_prefix('#')?;
    let channel = |at: usize| -> Option<f64> {
        let value = f64::from(u8::from_str_radix(digits.get(at..at + 2)?, 16).ok()?) / 255.0;
        Some(if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        })
    };
    Some(0.2126 * channel(0)? + 0.7152 * channel(2)? + 0.0722 * channel(4)?)
}

/// WCAG's contrast ratio between two colours.
fn contrast(a: &str, b: &str) -> Option<f64> {
    let (a, b) = (luminance(a)?, luminance(b)?);
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    Some((hi + 0.05) / (lo + 0.05))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_hex_colour_or_nothing_reaches_a_stylesheet() {
        assert_eq!(clean_colour(""), Some(None));
        assert_eq!(clean_colour("  "), Some(None));
        assert_eq!(clean_colour(" #2F6B4F "), Some(Some("#2f6b4f".to_string())));
        for bad in [
            "2f6b4f",
            "#2f6b4",
            "#2f6b4fa",
            "red",
            "#xxxxxx",
            "#000;}html{",
            "url(x)",
        ] {
            assert_eq!(clean_colour(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_default_style_needs_no_stylesheet() {
        assert_eq!(CheckoutStyle::default().css(), None);
    }

    /// A light scheme's colours mustn't reach a customer whose device is dark, or
    /// the other way around, nor beat a page pinned to the other.
    #[test]
    fn a_style_writes_only_what_it_changes_for_each_scheme() {
        let style = CheckoutStyle {
            light: Roles {
                accent: Some("#111111".to_string()),
                ..Roles::default()
            },
            dark: Roles {
                text: Some("#222222".to_string()),
                ..Roles::default()
            },
            ..CheckoutStyle::default()
        };
        assert_eq!(
            style.css().unwrap(),
            ":root[data-theme=\"light\"]{--accent:#111111;--accent-text:#111111;}\
             @media not (prefers-color-scheme:dark){:root:not([data-theme=\"dark\"]){--accent:#111111;--accent-text:#111111;}}\
             :root[data-theme=\"dark\"]{--ink:#222222;}\
             @media (prefers-color-scheme:dark){:root:not([data-theme=\"light\"]){--ink:#222222;}}"
        );
    }

    #[test]
    fn a_background_of_the_stores_own_brings_the_shade_off_it_along() {
        let style = CheckoutStyle {
            light: Roles {
                background: Some("#102030".to_string()),
                ..Roles::default()
            },
            ..CheckoutStyle::default()
        };
        assert!(style.css().unwrap().contains("--surface-sunken:color-mix("));
    }

    #[test]
    fn a_stored_value_that_isnt_a_colour_never_reaches_the_stylesheet() {
        let style = CheckoutStyle {
            light: Roles {
                accent: Some("red;}html{display:none".to_string()),
                ..Roles::default()
            },
            ..CheckoutStyle::default()
        };
        assert!(!style.css().unwrap().contains("html{display"));
    }

    #[test]
    fn shape_and_transparency_have_their_own_rules() {
        let style = CheckoutStyle {
            corners: Corners::Square,
            font: Font::Serif,
            transparent: true,
            ..CheckoutStyle::default()
        };
        assert_eq!(
            style.css().unwrap(),
            ":root{--radius-sm:0;--radius-md:0;--radius-lg:0;--font-ui:Georgia,\"Times New Roman\",serif;}html,body{background:transparent}"
        );
    }

    #[test]
    fn hard_to_read_quiet_text_is_warned_about() {
        let mut style = CheckoutStyle::default();
        assert_eq!(style.contrast_warnings(), []);
        style.light.quiet = Some("#8b9a91".to_string());
        assert_eq!(
            style.contrast_warnings(),
            [],
            "against the theme: not known"
        );
        style.light.background = Some("#fbfaf6".to_string());
        let warnings = style.contrast_warnings();
        assert_eq!(warnings.len(), 1);
        assert_eq!((warnings[0].0, warnings[0].1), ("Quiet text", "light"));
        assert!((warnings[0].2 - 2.8).abs() < 0.2, "{}", warnings[0].2);
        style.light.quiet = Some("#333333".to_string());
        assert_eq!(style.contrast_warnings(), []);
        style.dark.text = Some("#333333".to_string());
        style.dark.background = Some("#222222".to_string());
        assert_eq!(style.contrast_warnings()[0].0, "Text");
    }

    #[test]
    fn a_stylesheets_name_comes_from_its_content() {
        assert_eq!(stylesheet_hash("a").len(), 32);
        assert_eq!(stylesheet_hash("a"), stylesheet_hash("a"));
        assert_ne!(stylesheet_hash("a"), stylesheet_hash("b"));
    }

    #[test]
    fn a_style_round_trips_through_its_stored_form_and_reads_an_older_one() {
        let style = CheckoutStyle {
            corners: Corners::Round,
            font: Font::Mono,
            ..CheckoutStyle::default()
        };
        let stored = serde_json::to_string(&style).unwrap();
        assert_eq!(
            serde_json::from_str::<CheckoutStyle>(&stored).unwrap(),
            style
        );
        assert_eq!(
            serde_json::from_str::<CheckoutStyle>("{}").unwrap(),
            CheckoutStyle::default()
        );
    }
}
