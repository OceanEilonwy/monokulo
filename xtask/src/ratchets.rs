//! Checks that keep the test suite from growing back what the test audit
//! took out, run as xtask's own tests (so the Linux Rust tests job runs
//! them on every pull request) and by `cargo xtask ratchets`:
//!
//! - a Rust test whose only assertions are that rendered markup contains
//!   some text, which pins markup rather than behaviour;
//! - a browser test reading computed styles, element geometry or colours,
//!   which pins presentation rather than behaviour;
//! - a gallery group the coverage run requires that no spec photographs any
//!   more, which only the coverage run would notice.
//!
//! The first two keep a baseline of what was already there, which can only
//! shrink: a new finding fails, and so does a fixed one still in the
//! baseline, until `cargo xtask ratchets --prune` drops it. A finding that
//! is right stays out of both through the reasoned lists below.

use crate::support::{at, files_under, read_json, root, write_json, Exit};
use regex::Regex;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::Path,
    sync::LazyLock,
};
use syn::{
    punctuated::Punctuated,
    visit::{self, Visit},
};

pub(crate) const HELP: &str = "\
        ratchets [--prune]\n\
                      Check no test was added that only finds text in markup or reads styles, and that\n\
                      every required gallery group has a spec; --prune drops fixed baseline entries";

const HTML_BASELINE: &str = "xtask/ratchets/markup-only-tests.json";
const STYLE_BASELINE: &str = "xtask/ratchets/e2e-style-reads.json";

/// Markup-only tests that are right as they are, by a word in the test's
/// name, and why.
const MARKUP_ALLOWED: [(&str, &str); 7] = [
    (
        "escape",
        "escaping: the markup is what shows a value can't break out",
    ),
    (
        "escaping",
        "escaping: the markup is what shows a value can't break out",
    ),
    (
        "secret",
        "a secret never reaching the page is only seen in the markup",
    ),
    (
        "without_javascript",
        "the no-JavaScript contract: the markup is what such a browser gets",
    ),
    (
        "without_js",
        "the no-JavaScript contract: the markup is what such a browser gets",
    ),
    (
        "no_js",
        "the no-JavaScript contract: the markup is what such a browser gets",
    ),
    (
        "noscript",
        "the no-JavaScript contract: the markup is what such a browser gets",
    ),
];

/// Style and geometry reads that are what a spec tests, by file and rule,
/// and why.
const STYLE_ALLOWED: [(&str, &str, &str); 2] = [
    (
        "e2e/browser/tests/live-view.spec.js",
        "boundingBox",
        "dragging and scrubbing the timeline needs where its handles are",
    ),
    (
        "e2e/browser/tests/site-themes.spec.js",
        "getComputedStyle",
        "one theme reaching every surface is what it tests",
    ),
];

fn pruned(dir: &Path) -> bool {
    dir.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.') || ["target", "node_modules", "vendor"].contains(&n))
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// HTML's elements a page or a fragment is written with; a custom element's
/// name has a hyphen.
const TAGS: [&str; 58] = [
    "a", "article", "aside", "b", "body", "br", "button", "code", "dd", "details", "dialog", "div",
    "dl", "dt", "em", "fieldset", "footer", "form", "h1", "h2", "h3", "h4", "header", "html", "i",
    "iframe", "img", "input", "label", "legend", "li", "link", "main", "meta", "nav", "noscript",
    "ol", "option", "p", "path", "pre", "script", "section", "select", "small", "span", "strong",
    "summary", "svg", "table", "td", "ul", "textarea", "th", "time", "title", "tr", "style",
];

/// Whether a string looked for is markup: an element's tag or an
/// attribute's value.
fn markup(text: &str) -> bool {
    text.contains("=\"")
        || text.match_indices('<').any(|(at, _)| {
            let rest = text[at + 1..].strip_prefix('/').unwrap_or(&text[at + 1..]);
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            let after = rest[name.len()..].chars().next();
            !name.is_empty()
                && after.is_none_or(|c| [' ', '>', '/'].contains(&c))
                && (TAGS.contains(&name.as_str()) || name.contains('-'))
        })
}

/// The string literal an argument is, or that a `format!` it is starts with.
fn literal(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) => Some(s.value()),
        syn::Expr::Reference(r) => literal(&r.expr),
        syn::Expr::Paren(p) => literal(&p.expr),
        syn::Expr::Macro(m) => m
            .mac
            .parse_body_with(Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated)
            .ok()
            .and_then(|args| args.first().and_then(literal)),
        _ => None,
    }
}

/// Whether a condition only asks whether text contains text (negated or
/// joined with others that do), noting in `markup_seen` a look for markup.
fn contains_only(expr: &syn::Expr, markup_seen: &mut bool) -> bool {
    match expr {
        syn::Expr::MethodCall(call) if call.method == "contains" => {
            if call
                .args
                .first()
                .and_then(literal)
                .is_some_and(|t| markup(&t))
            {
                *markup_seen = true;
            }
            true
        }
        syn::Expr::Unary(syn::ExprUnary {
            op: syn::UnOp::Not(_),
            expr,
            ..
        }) => contains_only(expr, markup_seen),
        syn::Expr::Paren(p) => contains_only(&p.expr, markup_seen),
        syn::Expr::Binary(b) if matches!(b.op, syn::BinOp::And(_) | syn::BinOp::Or(_)) => {
            contains_only(&b.left, markup_seen) & contains_only(&b.right, markup_seen)
        }
        _ => false,
    }
}

/// A test body's assertions: how many, how many only ask whether text
/// contains text, and whether any looks for markup.
#[derive(Default)]
struct Assertions {
    all: usize,
    contains: usize,
    markup: bool,
}

impl Visit<'_> for Assertions {
    fn visit_macro(&mut self, mac: &syn::Macro) {
        let Some(name) = mac.path.segments.last().map(|s| s.ident.to_string()) else {
            return;
        };
        if !(name.starts_with("assert")
            || name.starts_with("prop_assert")
            || name.starts_with("debug_assert"))
        {
            return;
        }
        self.all += 1;
        if ["assert", "prop_assert", "debug_assert"].contains(&name.as_str()) {
            let condition = mac
                .parse_body_with(Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated)
                .ok()
                .and_then(|args| args.into_iter().next());
            if condition.is_some_and(|c| contains_only(&c, &mut self.markup)) {
                self.contains += 1;
            }
        }
    }
}

/// Every `#[test]` under `root` whose assertions all only ask whether text
/// contains text, at least one of them looking for markup; named by its
/// file and module path, as `cargo xtask test-summary` names ignored tests.
fn markup_only_tests(root: &Path) -> io::Result<Vec<String>> {
    struct Tests {
        modules: Vec<String>,
        file: String,
        found: Vec<String>,
    }
    impl Visit<'_> for Tests {
        fn visit_item_mod(&mut self, item: &syn::ItemMod) {
            self.modules.push(item.ident.to_string());
            visit::visit_item_mod(self, item);
            self.modules.pop();
        }
        fn visit_item_fn(&mut self, item: &syn::ItemFn) {
            let test = item
                .attrs
                .iter()
                .any(|a| a.path().segments.last().is_some_and(|s| s.ident == "test"));
            if test {
                let mut assertions = Assertions::default();
                assertions.visit_block(&item.block);
                if assertions.all > 0 && assertions.contains == assertions.all && assertions.markup
                {
                    let path = self
                        .modules
                        .iter()
                        .cloned()
                        .chain([item.sig.ident.to_string()])
                        .collect::<Vec<_>>()
                        .join("::");
                    self.found.push(format!("{} › {path}", self.file));
                }
            }
            visit::visit_item_fn(self, item);
        }
    }
    let mut tests = Tests {
        modules: Vec::new(),
        file: String::new(),
        found: Vec::new(),
    };
    for path in files_under(root, pruned)? {
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let Some(file) = fs::read_to_string(&path)
            .ok()
            .and_then(|text| syn::parse_file(&text).ok())
        else {
            continue;
        };
        tests.file = relative(root, &path);
        tests.visit_file(&file);
    }
    tests.found.sort();
    Ok(tests.found)
}

/// The markup-only tests no reasoned word in their name allows.
fn markup_findings(root: &Path) -> io::Result<BTreeMap<String, u64>> {
    let mut found = BTreeMap::new();
    for name in markup_only_tests(root)? {
        let leaf = name.rsplit("::").next().unwrap_or(&name);
        if !MARKUP_ALLOWED.iter().any(|(word, _)| leaf.contains(word)) {
            *found.entry(name).or_insert(0) += 1;
        }
    }
    Ok(found)
}

static STYLE_RULES: LazyLock<[(&str, Regex); 4]> = LazyLock::new(|| {
    [
        ("getComputedStyle", Regex::new(r"\bgetComputedStyle\b").unwrap()),
        ("boundingBox", Regex::new(r"\bboundingBox\b").unwrap()),
        ("toHaveCSS", Regex::new(r"\btoHaveCSS\b").unwrap()),
        (
            "colour",
            Regex::new(r#"['"`]#(?:[0-9a-fA-F]{3,4}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})['"`]|\b(?:rgba?|hsla?|oklch)\("#)
                .unwrap(),
        ),
    ]
});

/// How often each browser test file reads a style, a box or a colour,
/// keyed `file:rule`, past the reasoned allowances.
fn style_findings(root: &Path) -> io::Result<BTreeMap<String, u64>> {
    let mut found = BTreeMap::new();
    for path in files_under(&root.join("e2e"), pruned)? {
        if path
            .extension()
            .and_then(|e| e.to_str())
            .is_none_or(|e| !["js", "mjs", "ts"].contains(&e))
        {
            continue;
        }
        let file = relative(root, &path);
        let text = fs::read_to_string(&path).map_err(|e| at(&path, e))?;
        for (rule, pattern) in STYLE_RULES.iter() {
            if STYLE_ALLOWED
                .iter()
                .any(|(f, r, _)| *f == file && r == rule)
            {
                continue;
            }
            let count = pattern.find_iter(&text).count() as u64;
            if count > 0 {
                found.insert(format!("{file}:{rule}"), count);
            }
        }
    }
    Ok(found)
}

/// What a baseline doesn't accept (new findings, or more of one), and what
/// it still lists that is fixed.
fn compare(
    found: &BTreeMap<String, u64>,
    baseline: &BTreeMap<String, u64>,
) -> (Vec<String>, Vec<String>) {
    let added = found
        .iter()
        .filter(|(k, n)| **n > baseline.get(*k).copied().unwrap_or(0))
        .map(|(k, n)| format!("{k} ({n})"))
        .collect();
    let fixed = baseline
        .iter()
        .filter(|(k, n)| found.get(*k).copied().unwrap_or(0) < **n)
        .map(|(k, _)| k.clone())
        .collect();
    (added, fixed)
}

/// The baseline with only what is still found, never more than it was.
fn pruned_baseline(
    found: &BTreeMap<String, u64>,
    baseline: &BTreeMap<String, u64>,
) -> BTreeMap<String, u64> {
    baseline
        .iter()
        .filter_map(|(k, n)| {
            let now = found.get(k).copied().unwrap_or(0).min(*n);
            (now > 0).then(|| (k.clone(), now))
        })
        .collect()
}

/// One ratchet's problems, or with `prune` its baseline rewritten without
/// what is fixed.
fn ratchet(
    root: &Path,
    baseline_path: &str,
    found: &BTreeMap<String, u64>,
    what: &str,
    prune: bool,
) -> io::Result<Vec<String>> {
    let path = root.join(baseline_path);
    let baseline: BTreeMap<String, u64> = read_json(&path)?;
    let (added, fixed) = compare(found, &baseline);
    if prune {
        write_json(&path, &pruned_baseline(found, &baseline))?;
    }
    let mut problems: Vec<String> = added
        .into_iter()
        .map(|k| format!("{what}: {k}, which {baseline_path} doesn't list"))
        .collect();
    if !prune && !fixed.is_empty() {
        problems.push(format!(
            "{baseline_path} still lists {} fixed: run `cargo xtask ratchets --prune`: {}",
            fixed.len(),
            fixed.join(", ")
        ));
    }
    Ok(problems)
}

static CAPTURE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bcaptureCoverageStage\(").unwrap());
static GROUP_LITERAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\bgroup:\s*['"]([a-z0-9-]+)['"]"#).unwrap());
static GROUP_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bgroup:\s*([A-Za-z_]\w*)").unwrap());
static CONST_STRING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\bconst\s+([A-Za-z_]\w*)\s*=\s*['"]([a-z0-9-]+)['"]"#).unwrap());
static CONST_OBJECT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bconst\s+([A-Za-z_]\w*)\s*=\s*(\{[^;]*\})").unwrap());
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z_]\w*").unwrap());
static REQUIRED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\brequired:\s*\[([^\]]*)\]").unwrap());
static QUOTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"['"]([a-z0-9-]+)['"]"#).unwrap());

/// The text of a call's arguments, from just inside its opening bracket.
fn arguments(text: &str) -> &str {
    let mut depth = 1;
    for (i, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return &text[..i];
                }
            }
            _ => {}
        }
    }
    text
}

/// The gallery group coverage-screenshot.js's `guessGroup` gives a capture
/// that names none.
fn guessed_group(file: &str) -> &'static str {
    if file.contains("challenge") {
        "challenge"
    } else if file.contains("pos") || file.contains("fit") {
        "pos"
    } else {
        "checkout"
    }
}

/// The gallery groups a spec's captures land in.
fn captured_groups(file: &str, text: &str) -> BTreeSet<String> {
    let strings: BTreeMap<&str, &str> = CONST_STRING
        .captures_iter(text)
        .map(|c| (c.get(1).unwrap().as_str(), c.get(2).unwrap().as_str()))
        .collect();
    let objects: BTreeMap<&str, &str> = CONST_OBJECT
        .captures_iter(text)
        .map(|c| (c.get(1).unwrap().as_str(), c.get(2).unwrap().as_str()))
        .collect();
    let group_in = |args: &str| -> Option<String> {
        if let Some(c) = GROUP_LITERAL.captures(args) {
            return Some(c[1].to_string());
        }
        if let Some(name) = GROUP_NAME.captures(args).and_then(|c| strings.get(&c[1])) {
            return Some((*name).to_string());
        }
        None
    };
    let mut groups = BTreeSet::new();
    for found in CAPTURE.find_iter(text) {
        let args = arguments(&text[found.end()..]);
        let group = group_in(args).or_else(|| {
            // Options kept in a constant (`const shot = { group: ... }`).
            WORD.find_iter(args)
                .filter_map(|w| objects.get(w.as_str()))
                .find_map(|object| group_in(object))
        });
        groups.insert(group.unwrap_or_else(|| guessed_group(file).to_string()));
    }
    groups
}

/// Each gallery group coverage-browser.config.js requires that no spec it
/// runs photographs.
fn unfed_gallery_groups(root: &Path) -> io::Result<Vec<String>> {
    let browser = root.join("e2e/browser");
    let config_path = browser.join("coverage-browser.config.js");
    let config = fs::read_to_string(&config_path).map_err(|e| at(&config_path, e))?;
    let real_path = browser.join("real-binaries.config.js");
    let real = fs::read_to_string(&real_path).map_err(|e| at(&real_path, e))?;
    let required: Vec<String> = REQUIRED
        .captures(&config)
        .map(|c| {
            QUOTED
                .captures_iter(&c[1])
                .map(|q| q[1].to_string())
                .collect()
        })
        .unwrap_or_default();
    if required.is_empty() {
        return Err(io::Error::other(format!(
            "{} names no required gallery groups",
            relative(root, &config_path)
        )));
    }
    let mut fed = BTreeSet::new();
    for path in files_under(&browser.join("tests"), pruned)? {
        let file = relative(&browser.join("tests"), &path);
        // Only a spec the coverage run runs feeds its gallery.
        let run = file.ends_with(".spec.js")
            && (config.contains(&format!("'{file}'")) || real.contains(&format!("'{file}'")));
        if !run {
            continue;
        }
        let text = fs::read_to_string(&path).map_err(|e| at(&path, e))?;
        fed.extend(captured_groups(&file, &text));
    }
    Ok(required.into_iter().filter(|g| !fed.contains(g)).collect())
}

/// Every check's problems; with `prune`, the baselines drop what is fixed.
fn problems(root: &Path, prune: bool) -> io::Result<Vec<String>> {
    let mut problems = ratchet(
        root,
        HTML_BASELINE,
        &markup_findings(root)?,
        "a test only finds text in rendered markup (assert what it does instead, or name why in MARKUP_ALLOWED)",
        prune,
    )?;
    problems.extend(ratchet(
        root,
        STYLE_BASELINE,
        &style_findings(root)?,
        "a browser test reads styles, boxes or colours (assert what the page does instead, or name why in STYLE_ALLOWED)",
        prune,
    )?);
    problems.extend(unfed_gallery_groups(root)?.into_iter().map(|g| {
        format!("the coverage run requires gallery group {g}, which no spec it runs photographs")
    }));
    Ok(problems)
}

/// `cargo xtask ratchets [--prune]`.
pub(crate) fn ratchets(args: &[&str]) -> io::Result<Exit> {
    let prune = match args {
        [] => false,
        ["--prune"] => true,
        _ => return Err(io::Error::other("usage: cargo xtask ratchets [--prune]")),
    };
    let problems = problems(&root(), prune)?;
    for problem in &problems {
        eprintln!("{problem}");
    }
    Ok(Exit::passed(problems.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::Scratch;

    #[test]
    fn no_new_test_only_finds_text_in_rendered_markup() {
        let root = root();
        let problems = ratchet(
            &root,
            HTML_BASELINE,
            &markup_findings(&root).unwrap(),
            "markup only",
            false,
        )
        .unwrap();
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    #[test]
    fn no_new_browser_test_reads_styles_boxes_or_colours() {
        let root = root();
        let problems = ratchet(
            &root,
            STYLE_BASELINE,
            &style_findings(&root).unwrap(),
            "style read",
            false,
        )
        .unwrap();
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    #[test]
    fn every_required_gallery_group_has_a_spec_that_photographs_it() {
        assert_eq!(unfed_gallery_groups(&root()).unwrap(), Vec::<String>::new());
    }

    /// What the markup check flags and what it lets through.
    #[test]
    fn only_a_test_whose_every_assertion_finds_markup_is_flagged() {
        let scratch = Scratch::new("ratchets-markup");
        fs::write(
            scratch.join("lib.rs"),
            r##"
            #[test] fn pins_markup() { let html = page(); assert!(html.contains("<b>paid</b>")); assert!(!html.contains(r#"class="x""#), "{html}"); }
            mod views { #[tokio::test] async fn pins_a_format() { assert!(page().contains(&format!("<a href=\"{}\">", 1)) && page().contains("x")); } }
            #[test] fn checks_the_status_too() { assert_eq!(status(), 200); assert!(page().contains("<b>")); }
            #[test] fn finds_words_only() { assert!(page().contains("Paid")); }
            #[test] fn finds_usage_text() { assert!(error().contains("expected ^<blocks back>")); }
            #[test] fn says_nothing() { page(); }
            #[test] fn the_value_is_escaped() { assert!(page().contains("&lt;b&gt;")); }
            #[test] fn a_tag_is_escaped_in_a_title() { assert!(page().contains("<title>&lt;")); }
            fn helper() { assert!(page().contains("<b>")); }
            "##,
        )
        .unwrap();
        assert_eq!(
            markup_only_tests(scratch.path()).unwrap(),
            [
                "lib.rs › a_tag_is_escaped_in_a_title",
                "lib.rs › pins_markup",
                "lib.rs › views::pins_a_format"
            ]
        );
        let found = markup_findings(scratch.path()).unwrap();
        assert_eq!(
            found.keys().collect::<Vec<_>>(),
            ["lib.rs › pins_markup", "lib.rs › views::pins_a_format"]
        );
        // A baseline accepts what it lists, and asks to drop what is fixed.
        let baseline = BTreeMap::from([
            ("lib.rs › pins_markup".to_string(), 1),
            ("lib.rs › gone".to_string(), 1),
        ]);
        let (added, fixed) = compare(&found, &baseline);
        assert_eq!(added, ["lib.rs › views::pins_a_format (1)"]);
        assert_eq!(fixed, ["lib.rs › gone"]);
        assert_eq!(
            pruned_baseline(&found, &baseline),
            BTreeMap::from([("lib.rs › pins_markup".to_string(), 1)])
        );
    }

    /// Which captures land in which gallery group.
    #[test]
    fn a_capture_lands_in_its_named_or_guessed_group() {
        let text = r#"
            const GROUP = 'wallets';
            const shot = { group: 'engine', shapes: ['desktop'] };
            await captureCoverageStage(page, 'a', test.info(), { group: 'site' });
            await captureCoverageStage(page, 'b', test.info(), { group: GROUP, asIs: true });
            await captureCoverageStage(page, 'c', test.info(), shot);
            await captureCoverageStage(page.locator('#x'), `d-${w}`, test.info());
        "#;
        let groups: Vec<String> = captured_groups("pos-terminal.spec.js", text)
            .into_iter()
            .collect();
        assert_eq!(groups, ["engine", "pos", "site", "wallets"]);
        assert_eq!(guessed_group("connection-challenge.spec.js"), "challenge");
        assert_eq!(guessed_group("checkout.spec.js"), "checkout");
    }
}
