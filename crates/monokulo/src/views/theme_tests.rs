//! Keeps every page on the one palette in `theme.css`: the site, the POS
//! terminal and the checkout used to define colours each on their own and
//! drifted apart (a black nav beside a cream POS bar, three looks for an
//! order's status, a `--warn` token nothing declared). These tests fail when
//! that starts again.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::{SITE_CSS, THEME_CSS};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn files_with_extension(dir: &Path, extension: &str) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == extension))
        // This file quotes bad CSS on purpose.
        .filter(|path| !path.ends_with("theme_tests.rs"))
        .collect();
    files.sort();
    files
}

/// Every `const *STYLE*: &str = r#"..."#;` in a views source file: the pages'
/// own style blocks.
fn style_consts(source: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = source;
    while let Some(start) = rest.find("const ") {
        rest = &rest[start + "const ".len()..];
        let Some(colon) = rest.find(':') else { break };
        let name = &rest[..colon];
        let Some(open) = rest.find("r#\"") else { break };
        if !name.contains("STYLE")
            || name.contains(char::is_whitespace)
            || rest[colon..open].trim() != ": &str ="
        {
            continue;
        }
        let body = &rest[open + 3..];
        let end = body.find("\"#").expect("an unterminated raw string");
        found.push(body[..end].to_string());
        rest = &body[end..];
    }
    found
}

/// Every stylesheet a page can load, named for failure messages. `theme.css`
/// is the one place allowed to hold colour values.
fn stylesheets() -> Vec<(String, String)> {
    let mut sheets = vec![
        ("views/site.css".to_string(), SITE_CSS.to_string()),
        (
            "pos-ui/src/pos.css".to_string(),
            read(&crate_dir().join("pos-ui/src/pos.css")),
        ),
    ];
    for path in files_with_extension(&crate_dir().join("src/views"), "rs") {
        for (i, style) in style_consts(&read(&path)).into_iter().enumerate() {
            sheets.push((
                format!(
                    "{} (style block {})",
                    path.file_name().unwrap().to_string_lossy(),
                    i + 1
                ),
                style,
            ));
        }
    }
    // The GitHub Pages site (web/) loads theme.css too: the landing page and
    // the quality report.
    for page in ["index.html", "quality/index.html"] {
        let text = read(&crate_dir().join("../../web").join(page));
        for (i, block) in text.split("<style>").skip(1).enumerate() {
            let end = block.find("</style>").expect("an unterminated <style>");
            sheets.push((
                format!("web/{page} (style block {})", i + 1),
                block[..end].to_string(),
            ));
        }
    }
    sheets
}

/// Markup that can carry colours of its own: the views and the POS app.
fn markup_sources() -> Vec<(String, String)> {
    let mut sources = Vec::new();
    for (dir, ext) in [
        ("src/views", "rs"),
        ("pos-ui/src", "tsx"),
        ("pos-ui/src", "ts"),
    ] {
        for path in files_with_extension(&crate_dir().join(dir), ext) {
            sources.push((
                format!("{dir}/{}", path.file_name().unwrap().to_string_lossy()),
                read(&path),
            ));
        }
    }
    sources
}

fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        match rest[start..].find("*/") {
            Some(end) => rest = &rest[start + end + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// `(property, value)` for every declaration. Selectors with a pseudo-class
/// (`a:hover`) come through as a harmless pair too; nothing in them looks
/// like a colour.
fn declarations(css: &str) -> Vec<(String, String)> {
    strip_comments(css)
        .split(['{', '}', ';'])
        .filter_map(|segment| {
            let (property, value) = segment.split_once(':')?;
            let property = property.trim();
            let is_property = !property.is_empty()
                && property
                    .trim_start_matches('-')
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-');
            is_property.then(|| (property.to_string(), value.trim().to_string()))
        })
        .collect()
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

const NAMED_COLOURS: &[&str] = &[
    "white", "black", "red", "green", "blue", "gray", "grey", "orange", "yellow", "purple", "pink",
    "brown", "silver", "navy", "teal", "maroon", "olive", "lime", "aqua", "fuchsia",
];

/// The colour literals in one CSS value (or markup snippet): hex, `rgb()`/
/// `hsl()` and named colours.
fn colour_literals(value: &str) -> Vec<String> {
    let mut found = Vec::new();
    let chars: Vec<char> = value.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == '#' {
            let hex: String = chars[i + 1..]
                .iter()
                .take_while(|c| c.is_ascii_hexdigit())
                .collect();
            let next = chars.get(i + 1 + hex.len()).copied();
            if matches!(hex.len(), 3 | 4 | 6 | 8) && !next.is_some_and(is_ident) {
                found.push(format!("#{hex}"));
            }
        }
    }
    let lower = value.to_ascii_lowercase();
    for function in ["rgb(", "rgba(", "hsl(", "hsla("] {
        let mut from = 0;
        while let Some(at) = lower[from..].find(function) {
            let at = from + at;
            if !lower[..at].ends_with(is_ident) {
                found.push(function.to_string());
            }
            from = at + function.len();
        }
    }
    for word in lower.split(|c: char| !is_ident(c)) {
        if NAMED_COLOURS.contains(&word) {
            found.push(word.to_string());
        }
    }
    found
}

#[test]
fn no_colour_is_defined_outside_theme_css() {
    let mut problems = Vec::new();
    for (name, css) in stylesheets() {
        for (property, value) in declarations(&css) {
            // `color-scheme: light dark` and the like name themes, not colours.
            if property == "color-scheme" {
                continue;
            }
            for literal in colour_literals(&value) {
                problems.push(format!("{name}: `{property}: {value}` uses {literal}"));
            }
        }
    }
    // Colours written straight into markup (`fill="#ff6600"`, `style="color: red"`).
    for (name, source) in markup_sources() {
        for attribute in [
            "fill=\"",
            "stroke=\"",
            "stop-color=\"",
            "color=\"",
            "style=\"",
        ] {
            for (at, _) in source.match_indices(attribute) {
                let value_start = at + attribute.len();
                let value = &source
                    [value_start..value_start + source[value_start..].find('"').unwrap_or(0)];
                let value = if attribute == "style=\"" {
                    value.to_string()
                } else {
                    format!("x: {value}")
                };
                for (_, declared) in declarations(&value) {
                    for literal in colour_literals(&declared) {
                        problems.push(format!("{name}: {attribute}{value}\" uses {literal}"));
                    }
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "colours belong in views/theme.css (add a role there and use it):\n{}",
        problems.join("\n")
    );
}

fn custom_properties_declared(text: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i + 2 < chars.len() {
        let starts_name =
            chars[i] == '-' && chars[i + 1] == '-' && (i == 0 || !is_ident(chars[i - 1]));
        if starts_name {
            let name: String = chars[i..].iter().take_while(|&&c| is_ident(c)).collect();
            let after = chars[i + name.chars().count()..]
                .iter()
                .find(|c| !c.is_whitespace() && **c != '\'' && **c != '"');
            if name.len() > 2 && after == Some(&':') {
                names.insert(name.clone());
            }
            i += name.chars().count().max(1);
        } else {
            i += 1;
        }
    }
    names
}

fn custom_properties_used(text: &str) -> BTreeSet<String> {
    text.match_indices("var(--")
        .map(|(at, _)| {
            text[at + 4..]
                .chars()
                .take_while(|&c| is_ident(c))
                .collect()
        })
        .collect()
}

#[test]
fn every_token_a_page_uses_is_declared() {
    let mut texts = vec![("views/theme.css".to_string(), strip_comments(THEME_CSS))];
    texts.extend(
        stylesheets()
            .into_iter()
            .map(|(name, css)| (name, strip_comments(&css))),
    );
    texts.extend(markup_sources());
    let declared: BTreeSet<String> = texts
        .iter()
        .flat_map(|(_, text)| custom_properties_declared(text))
        .collect();
    let mut undeclared = Vec::new();
    for (name, text) in &texts {
        for used in custom_properties_used(text) {
            if !declared.contains(&used) {
                undeclared.push(format!("{name}: var({used}) is never declared"));
            }
        }
    }
    assert!(
        undeclared.is_empty(),
        "a `var(--x, fallback)` whose token doesn't exist always shows its fallback:\n{}",
        undeclared.join("\n")
    );
}

/// The declarations inside the block that opens with `opening` (which ends
/// with its `{`).
fn block(css: &str, opening: &str) -> BTreeMap<String, String> {
    let start = css
        .find(opening)
        .unwrap_or_else(|| panic!("theme.css has no `{opening}`"))
        + opening.len();
    let end = start + css[start..].find('}').expect("an unclosed block");
    declarations(&css[start..end])
        .into_iter()
        .filter(|(name, _)| name.starts_with("--"))
        .collect()
}

fn light_tokens() -> BTreeMap<String, String> {
    block(THEME_CSS, ":root {")
}

fn dark_overrides() -> BTreeMap<String, String> {
    block(THEME_CSS, ":root[data-theme=\"dark\"] {")
}

/// The browser draws a number box's step arrows, a select's list and
/// scroll bars itself, from `color-scheme`: it has to follow the theme in
/// use, or an account that chose light on a dark OS gets dark arrows.
#[test]
fn the_browsers_own_control_parts_follow_the_theme_in_use() {
    let css = strip_comments(THEME_CSS);
    let scheme = |opening: &str| {
        let start = css
            .find(opening)
            .unwrap_or_else(|| panic!("theme.css has no `{opening}`"))
            + opening.len();
        let end = start + css[start..].find('}').expect("an unclosed block");
        declarations(&css[start..end])
            .into_iter()
            .find(|(name, _)| name == "color-scheme")
            .map(|(_, value)| value)
    };
    assert_eq!(scheme(":root {").as_deref(), Some("light"));
    assert_eq!(
        scheme(":root:not([data-theme=\"light\"]) {").as_deref(),
        Some("dark")
    );
    assert_eq!(
        scheme(":root[data-theme=\"dark\"] {").as_deref(),
        Some("dark")
    );
}

#[test]
fn both_dark_blocks_agree_and_only_override_light_tokens() {
    let css = strip_comments(THEME_CSS);
    let from_os = block(&css, ":root:not([data-theme=\"light\"]) {");
    let chosen = dark_overrides();
    assert_eq!(
        from_os, chosen,
        "the prefers-color-scheme block and the data-theme=\"dark\" block must say the same thing"
    );
    let light = light_tokens();
    let only_dark: Vec<&String> = chosen
        .keys()
        .filter(|name| !light.contains_key(*name))
        .collect();
    assert!(
        only_dark.is_empty(),
        "a token only a dark block declares doesn't exist in light mode: {only_dark:?}"
    );
}

fn resolve(tokens: &BTreeMap<String, String>, value: &str) -> String {
    let mut value = value.trim().to_string();
    for _ in 0..10 {
        match value.strip_prefix("var(").and_then(|v| v.strip_suffix(')')) {
            Some(name) => {
                value = tokens
                    .get(name.trim())
                    .unwrap_or_else(|| panic!("{name} isn't a theme token"))
                    .trim()
                    .to_string()
            }
            None => return value,
        }
    }
    panic!("{value}: too many var() hops")
}

fn luminance(hex: &str) -> f64 {
    let hex = hex.trim_start_matches('#');
    assert_eq!(
        hex.len(),
        6,
        "contrast needs a 6-digit hex colour, got #{hex}"
    );
    let channel = |i: usize| {
        let c = u8::from_str_radix(&hex[i..i + 2], 16).unwrap() as f64 / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(0) + 0.7152 * channel(2) + 0.0722 * channel(4)
}

fn contrast(a: &str, b: &str) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

#[test]
fn text_and_controls_are_legible_in_both_themes() {
    // (foreground, background, minimum): 4.5:1 for text, 3:1 for the edge
    // of a control (WCAG 1.4.3, 1.4.11).
    let mut pairs: Vec<(String, String, f64)> = [
        ("--ink", "--paper", 4.5),
        ("--ink", "--paper-raised", 4.5),
        ("--muted", "--paper", 4.5),
        ("--muted", "--paper-raised", 4.5),
        ("--chrome-ink", "--chrome-bg", 4.5),
        ("--table-head-ink", "--table-head-bg", 4.5),
        ("--btn-ink", "--btn-bg", 4.5),
        ("--btn-ink", "--btn-hover-bg", 4.5),
        ("--btn-primary-ink", "--btn-primary-bg", 4.5),
        ("--btn-primary-ink", "--btn-primary-hover", 4.5),
        ("--accent-text", "--paper", 4.5),
        ("--accent-text", "--paper-raised", 4.5),
        ("--accent-text", "--chrome-bg", 4.5),
        ("--error", "--paper-raised", 4.5),
        ("--warning", "--paper-raised", 4.5),
        ("--success", "--paper-raised", 4.5),
        ("--control-border", "--control-bg", 3.0),
        ("--control-border", "--paper", 3.0),
        ("--btn-border", "--btn-bg", 3.0),
        ("--focus-ring", "--paper", 3.0),
        // The resource charts' layers are graphics (WCAG 1.4.11).
        ("--chart-engine", "--paper-raised", 3.0),
        ("--chart-monokulo", "--paper-raised", 3.0),
    ]
    .iter()
    .map(|(fg, bg, min)| (fg.to_string(), bg.to_string(), *min))
    .collect();
    for state in [
        "pending",
        "unconfirmed",
        "confirming",
        "partial",
        "paid",
        "overpaid",
        "expired",
        "double-spend",
        "cancelled",
    ] {
        pairs.push((
            format!("--state-{state}-ink"),
            format!("--state-{state}-bg"),
            4.5,
        ));
    }

    let light = light_tokens();
    let mut dark = light.clone();
    dark.extend(dark_overrides());
    let mut failures = Vec::new();
    for (theme, tokens) in [("light", &light), ("dark", &dark)] {
        for (fg, bg, min) in &pairs {
            let (fg_value, bg_value) = (
                resolve(tokens, &format!("var({fg})")),
                resolve(tokens, &format!("var({bg})")),
            );
            let ratio = contrast(&fg_value, &bg_value);
            if ratio < *min {
                failures.push(format!("{theme}: {fg} ({fg_value}) on {bg} ({bg_value}) is {ratio:.2}:1, needs {min}:1"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_checks_catch_what_they_are_for() {
    assert_eq!(colour_literals("1px solid #111"), vec!["#111"]);
    assert_eq!(colour_literals("var(--warn, #9a6700)"), vec!["#9a6700"]);
    assert_eq!(colour_literals("rgba(0, 0, 0, .2)"), vec!["rgba("]);
    assert_eq!(colour_literals("white"), vec!["white"]);
    assert!(colour_literals("var(--paper-raised)").is_empty());
    assert!(colour_literals("nowrap").is_empty());
    assert_eq!(
        declarations("#pos-root .x:hover { color: red; } a:hover{b:c}"),
        vec![
            ("color".to_string(), "red".to_string()),
            ("a".to_string(), "hover".to_string()),
            ("b".to_string(), "c".to_string()),
        ]
    );
    assert!(custom_properties_used("a { color: var(--warn, #9a6700) }").contains("--warn"));
    assert!(!custom_properties_declared(":root { --warning: #7d5e00; }").contains("--warn"));
    assert!(custom_properties_declared("style={{ '--progress': x }}").contains("--progress"));
    assert!((contrast("#000000", "#ffffff") - 21.0).abs() < 0.01);
}
