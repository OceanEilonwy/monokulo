//! A page's CSS, read the way browsers read it and written back scoped.
//!
//! Nothing here is the last line of defence: the page's Content-Security-Policy
//! (`monokulo::views::checkout`) stops anything loading from another site
//! whatever the CSS says. This is what tells a merchant so at save time rather
//! than leaving them to find it in a browser's console, and what limits
//! accidents: a selector can only reach what's inside the page, by the classes
//! the template has or `data-part`.
//!
//! Tokens come from `cssparser`, so escapes are already undone: `u\72 l(` is a
//! `url(` here, as it would be in a browser. What's written back is written
//! from those tokens, never copied from the source.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use cssparser::{Parser, ToCss, Token, TokenSerializationType};

use crate::feature::ALL_FEATURES;
use crate::{Place, Problem, MAX_PROBLEMS};

/// The largest CSS accepted, in bytes, before it is even read.
pub const MAX_CSS_BYTES: usize = 32 * 1024;
/// How deeply blocks and brackets may nest.
const MAX_NESTING: usize = 16;

/// The classes any page may use, on top of its template's own.
pub const STOCK_CLASSES: [&str; 7] = [
    "mk-row",
    "mk-stack",
    "mk-center",
    "mk-card",
    "mk-gap-sm",
    "mk-gap-md",
    "mk-gap-lg",
];

/// What everything is scoped under: the element the template renders in.
const SCOPE: &str = "#checkout-root";

/// Rewrites `css`, scoped to the page, or says what's wrong with it.
/// `classes` are the template's, which with [`STOCK_CLASSES`] are all a
/// selector may name.
pub fn sanitize(css: &str, classes: &BTreeSet<String>) -> Result<String, Vec<Problem>> {
    if css.len() > MAX_CSS_BYTES {
        return Err(vec![problem(
            1,
            format!(
                "This CSS is over the limit of {} KiB.",
                MAX_CSS_BYTES / 1024
            ),
            None,
        )]);
    }
    let css = crate::normalize(css);
    // A browser drops a stylesheet's byte order mark, where a parser reads it
    // as part of the first selector.
    let mut parser = Parser::new(css.strip_prefix('\u{feff}').unwrap_or(&css));
    let mut sanitizer = Sanitizer {
        classes,
        problems: Vec::new(),
    };
    let mut out = Writer::default();
    sanitizer.rules(&mut parser, &mut out, 0);
    if sanitizer.problems.is_empty() {
        // Checked as a browser would read it, whatever the rules above let by:
        // this is what the page's scoping rests on.
        if written_is_safe(&out.text) {
            Ok(out.text)
        } else {
            Err(vec![problem(
                1,
                "This CSS can't be used safely.".to_string(),
                Some("Simplify anything unusual: escapes, or brackets in values."),
            )])
        }
    } else {
        sanitizer.problems.truncate(MAX_PROBLEMS);
        Err(sanitizer.problems)
    }
}

fn problem(line: u32, message: String, hint: Option<&str>) -> Problem {
    Problem {
        place: Place::Css(line),
        message,
        hint: hint.map(str::to_string),
    }
}

fn line_of(parser: &Parser) -> u32 {
    parser.current_source_location().line + 1
}

/// Writes tokens so they read back as the same tokens, with an empty comment
/// where two would otherwise run together. Whitespace is held back until
/// something follows it, so none is left to trim: editing what's written is
/// how a trailing `\\ ` would come to escape the `{` that follows it.
#[derive(Default)]
struct Writer {
    text: String,
    last: Option<TokenSerializationType>,
    space: bool,
}

impl Writer {
    fn token(&mut self, token: &Token) {
        if matches!(token, Token::WhiteSpace(_)) {
            self.space = !self.text.is_empty();
            return;
        }
        if std::mem::take(&mut self.space) {
            self.text.push(' ');
            self.last = None;
        }
        let kind = token.serialization_type();
        if self
            .last
            .is_some_and(|last| last.needs_separator_when_before(kind))
        {
            self.text.push_str("/**/");
        }
        // Writing to a `String` can't fail.
        let _ = token.to_css(&mut self.text);
        self.last = Some(kind);
    }

    /// Text that starts something new, which keeps the space before it: a
    /// descendant selector means something else without one.
    fn start(&mut self, text: &str) {
        if std::mem::take(&mut self.space) {
            self.text.push(' ');
        }
        self.raw(text);
    }

    fn raw(&mut self, text: &str) {
        self.space = false;
        self.text.push_str(text);
        self.last = None;
    }
}

struct Sanitizer<'a> {
    classes: &'a BTreeSet<String>,
    problems: Vec<Problem>,
}

/// The at-rules allowed, each with blocks of rules inside. Anything else loads
/// something (`@import`, `@font-face`) or reaches beyond the page (`@layer`,
/// which could outrank what keeps a code scannable; `@namespace`; `@page`).
const GROUP_AT_RULES: [&str; 3] = ["media", "supports", "container"];

/// Functions that load something, or could, or paint what's elsewhere on the
/// page, with or without a vendor's prefix.
fn forbidden_function(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let bare = ["-webkit-", "-moz-", "-ms-", "-o-"]
        .into_iter()
        .find_map(|prefix| name.strip_prefix(prefix))
        .unwrap_or(&name);
    matches!(
        bare,
        "url"
            | "src"
            | "image"
            | "image-set"
            | "image-rect"
            | "cross-fade"
            | "element"
            | "paint"
            | "canvas"
    )
}

/// Properties a page can't set.
fn forbidden_property(name: &str) -> Option<&'static str> {
    match name {
        "behavior" | "-moz-binding" => Some("It runs code in old browsers."),
        "--qr-bg" => Some("The code stays dark on white, so wallets can scan it."),
        name if name.starts_with("--state-") => {
            Some("Order status colours stay as they are, so they stay readable.")
        }
        _ => None,
    }
}

impl Sanitizer<'_> {
    fn add(&mut self, line: u32, message: String, hint: Option<&str>) {
        if self.problems.len() < MAX_PROBLEMS {
            self.problems.push(problem(line, message, hint));
        }
    }

    /// A list of rules, at the top or inside a group.
    fn rules<'i>(&mut self, parser: &mut Parser<'i>, out: &mut Writer, nesting: usize) {
        loop {
            if self.problems.len() >= MAX_PROBLEMS {
                return;
            }
            // `next` skips whitespace and comments, which are never written.
            let start = parser.state();
            let Ok(token) = parser.next().cloned() else {
                return;
            };
            let line = line_of(parser);
            match token {
                Token::AtKeyword(name) => {
                    self.at_rule(parser, out, name.as_ref(), line, nesting);
                }
                // Left by an old browser's HTML comment syntax.
                Token::CDO | Token::CDC => {}
                _ => {
                    parser.reset(&start);
                    self.qualified_rule(parser, out, line, nesting);
                }
            }
        }
    }

    fn at_rule<'i>(
        &mut self,
        parser: &mut Parser<'i>,
        out: &mut Writer,
        name: &str,
        line: u32,
        nesting: usize,
    ) {
        let lower = name.to_ascii_lowercase();
        if lower == "keyframes" {
            return self.keyframes(parser, out, line, nesting);
        }
        if !GROUP_AT_RULES.contains(&lower.as_str()) {
            let hint = match lower.as_str() {
                "import" | "font-face" => {
                    Some("Custom pages load nothing from other sites, so customers on Tor stay private. Fonts can come from the customer's device.")
                }
                _ => None,
            };
            self.add(line, format!("@{name} can't be used."), hint);
            skip_rule(parser);
            return;
        }
        if nesting >= MAX_NESTING {
            self.add(line, "This CSS nests too deeply.".to_string(), None);
            skip_rule(parser);
            return;
        }
        out.raw(&format!("@{lower} "));
        parser.skip_whitespace();
        if !self.prelude(parser, out, line, nesting) {
            return;
        }
        out.raw("{");
        let result: Result<(), cssparser::ParseError<()>> = parser.parse_nested_block(|parser| {
            self.rules(parser, out, nesting + 1);
            Ok(())
        });
        let _ = result;
        out.raw("}");
    }

    /// Copies what comes before a block. `false` if no block followed.
    fn prelude<'i>(
        &mut self,
        parser: &mut Parser<'i>,
        out: &mut Writer,
        line: u32,
        nesting: usize,
    ) -> bool {
        loop {
            let Ok(token) = parser.next_including_whitespace().cloned() else {
                self.add(line, "This rule has no { }.".to_string(), None);
                return false;
            };
            match token {
                Token::CurlyBracketBlock => return true,
                Token::Semicolon => {
                    self.add(line, "This rule has no { }.".to_string(), None);
                    return false;
                }
                token => self.value_token(parser, out, &token, nesting),
            }
        }
    }

    fn keyframes<'i>(
        &mut self,
        parser: &mut Parser<'i>,
        out: &mut Writer,
        line: u32,
        nesting: usize,
    ) {
        let Ok(name) = parser.expect_ident_cloned() else {
            self.add(line, "@keyframes needs a name.".to_string(), None);
            skip_rule(parser);
            return;
        };
        if parser.expect_curly_bracket_block().is_err() {
            self.add(line, "This rule has no { }.".to_string(), None);
            return;
        }
        out.raw("@keyframes ");
        out.token(&Token::Ident(name));
        out.raw("{");
        let result: Result<(), cssparser::ParseError<()>> =
            parser.parse_nested_block(|parser| loop {
                let line = line_of(parser);
                let mut any = false;
                loop {
                    let Ok(token) = parser.next().cloned() else {
                        if any {
                            self.add(line, "This keyframe has no { }.".to_string(), None);
                        }
                        return Ok(());
                    };
                    match token {
                        Token::CurlyBracketBlock => break,
                        Token::Percentage { .. } | Token::Comma => out.token(&token),
                        Token::Ident(ref word)
                            if word.eq_ignore_ascii_case("from")
                                || word.eq_ignore_ascii_case("to") =>
                        {
                            out.token(&token)
                        }
                        token => self.add(
                            line,
                            format!(
                                "A keyframe is from, to or a percentage, not {}.",
                                token.to_css_string()
                            ),
                            None,
                        ),
                    }
                    any = true;
                }
                out.raw("{");
                let _: Result<(), cssparser::ParseError<()>> =
                    parser.parse_nested_block(|parser| {
                        self.declarations(parser, out, nesting + 1);
                        Ok(())
                    });
                out.raw("}");
            });
        let _ = result;
        out.raw("}");
    }

    /// `selector, selector { declarations }`.
    fn qualified_rule<'i>(
        &mut self,
        parser: &mut Parser<'i>,
        out: &mut Writer,
        line: u32,
        nesting: usize,
    ) {
        let mut selectors = vec![Writer::default()];
        loop {
            let Ok(token) = parser.next_including_whitespace().cloned() else {
                self.add(line, "This rule has no { }.".to_string(), None);
                return;
            };
            match token {
                Token::CurlyBracketBlock => break,
                Token::Semicolon => {
                    self.add(line, "This rule has no { }.".to_string(), None);
                    return;
                }
                Token::Comma => selectors.push(Writer::default()),
                token => {
                    let current = selectors.last_mut().expect("starts with one");
                    // After the scope, these would reach its siblings or itself.
                    if current.text.is_empty() {
                        if let Token::Delim(c @ ('~' | '+' | '>' | '|')) = token {
                            self.add(
                                line,
                                format!("A selector can't start with {c}."),
                                Some("Everything is inside your page already."),
                            );
                            continue;
                        }
                    }
                    self.selector_token(parser, current, &token, line, nesting);
                }
            }
        }
        for (i, selector) in selectors.iter().enumerate() {
            if i > 0 {
                out.raw(",");
            }
            if selector.text.is_empty() {
                self.add(line, "A selector is missing here.".to_string(), None);
            }
            let _ = write!(out.text, "{SCOPE} {}", selector.text);
        }
        out.raw("{");
        let _: Result<(), cssparser::ParseError<()>> = parser.parse_nested_block(|parser| {
            self.declarations(parser, out, nesting + 1);
            Ok(())
        });
        out.raw("}");
    }

    fn selector_token<'i>(
        &mut self,
        parser: &mut Parser<'i>,
        out: &mut Writer,
        token: &Token<'i>,
        line: u32,
        nesting: usize,
    ) {
        if nesting > MAX_NESTING {
            self.add(line, "This CSS nests too deeply.".to_string(), None);
            return;
        }
        match token {
            Token::Delim('&') => self.add(
                line,
                "& can't be used: a rule can't go inside a rule.".to_string(),
                None,
            ),
            Token::Delim('\\') => self.add(line, "A stray \\ can't be used.".to_string(), None),
            Token::IDHash(id) | Token::Hash(id) => self.add(
                line,
                format!("#{id} can't be selected: ids are the page's own."),
                Some("Use a class from your template, or [data-part=\"qr\"]."),
            ),
            Token::Delim('.') => {
                let name = parser.next_including_whitespace().cloned();
                match name {
                    Ok(Token::Ident(class)) => {
                        let class: &str = class.as_ref();
                        if !self.classes.contains(class) && !STOCK_CLASSES.contains(&class) {
                            self.add(
                                line,
                                format!(".{class} isn't a class in your template."),
                                Some("A page's own classes can change between releases, so only your template's and the mk- classes can be selected. Use [data-part=\"qr\"] for a feature."),
                            );
                        }
                        out.token(token);
                        out.token(&Token::Ident(class.into()));
                    }
                    _ => self.add(line, "A . here needs a class name.".to_string(), None),
                }
            }
            Token::Colon => {
                out.token(token);
                // `::before` or `:hover`.
                let Ok(next) = parser.next_including_whitespace().cloned() else {
                    return;
                };
                match next {
                    Token::Colon => {
                        out.token(&next);
                        match parser.next_including_whitespace().cloned() {
                            Ok(Token::Colon) => {
                                self.add(line, "This selector can't be read.".to_string(), None)
                            }
                            Ok(after) => {
                                self.selector_token(parser, out, &after, line, nesting + 1);
                            }
                            Err(_) => {}
                        }
                    }
                    Token::Ident(ref word) if word.eq_ignore_ascii_case("root") => self.add(
                        line,
                        ":root can't be selected: everything is inside your page already."
                            .to_string(),
                        None,
                    ),
                    next => self.selector_token(parser, out, &next, line, nesting + 1),
                }
            }
            Token::Ident(word)
                if word.eq_ignore_ascii_case("html") || word.eq_ignore_ascii_case("body") =>
            {
                self.add(
                    line,
                    format!("{word} can't be selected: everything is inside your page already."),
                    None,
                );
            }
            Token::SquareBracketBlock => self.attribute_selector(parser, out, line),
            Token::Function(name) => {
                if forbidden_function(name) {
                    self.add(line, format!("{name}() can't be used."), None);
                    return;
                }
                if nesting >= MAX_NESTING {
                    self.add(line, "This CSS nests too deeply.".to_string(), None);
                    return;
                }
                out.token(token);
                let _: Result<(), cssparser::ParseError<()>> =
                    parser.parse_nested_block(|parser| {
                        while let Ok(inner) = parser.next_including_whitespace().cloned() {
                            if matches!(inner, Token::Comma) {
                                out.token(&inner);
                            } else {
                                self.selector_token(parser, out, &inner, line, nesting + 1);
                            }
                        }
                        Ok(())
                    });
                out.raw(")");
            }
            Token::UnquotedUrl(_) | Token::BadUrl(_) | Token::BadString(_) => {
                self.add(line, "This selector can't be read.".to_string(), None);
            }
            Token::CurlyBracketBlock | Token::ParenthesisBlock => {
                self.add(line, "This selector can't be read.".to_string(), None);
            }
            Token::Ident(_)
            | Token::WhiteSpace(_)
            | Token::Delim(_)
            | Token::Number { .. }
            | Token::Dimension { .. }
            | Token::Percentage { .. }
            | Token::IncludeMatch
            | Token::DashMatch
            | Token::PrefixMatch
            | Token::SubstringMatch
            | Token::SuffixMatch => out.token(token),
            token => self.add(
                line,
                format!("{} can't be in a selector.", token.to_css_string()),
                None,
            ),
        }
    }

    /// `[data-part="qr"]`: the only attribute that can be selected, so a page
    /// can't read the order's other attributes by selector.
    fn attribute_selector<'i>(&mut self, parser: &mut Parser<'i>, out: &mut Writer, line: u32) {
        let parsed: Result<Option<String>, cssparser::ParseError<()>> =
            parser.parse_nested_block(|parser| {
                let name = parser.expect_ident_cloned()?;
                if !name.eq_ignore_ascii_case("data-part") {
                    return Ok(None);
                }
                parser.expect_delim('=')?;
                let value: &str = parser.expect_ident_or_string()?;
                let value = value.to_string();
                parser.expect_exhausted()?;
                Ok(Some(value))
            });
        match parsed {
            Ok(Some(value)) => {
                if !ALL_FEATURES.iter().any(|feature| feature.name() == value) {
                    self.add(line, format!("\"{value}\" isn't a data-part."), None);
                    return;
                }
                out.start("[data-part=\"");
                out.raw(&value);
                out.raw("\"]");
            }
            _ => self.add(
                line,
                "Only [data-part=\"...\"] can be selected, by its exact value.".to_string(),
                Some("A page's other attributes can change between releases."),
            ),
        }
    }

    /// `name: value; ...`, inside a rule.
    fn declarations<'i>(&mut self, parser: &mut Parser<'i>, out: &mut Writer, nesting: usize) {
        loop {
            let Ok(token) = parser.next().cloned() else {
                return;
            };
            let line = line_of(parser);
            match token {
                Token::Semicolon => {}
                Token::Ident(name) => {
                    let name: String = name.as_ref().to_string();
                    let lower = name.to_ascii_lowercase();
                    if parser.expect_colon().is_err() {
                        self.add(
                            line,
                            format!("{name} needs a : here. A rule can't go inside a rule."),
                            None,
                        );
                        skip_declaration(parser);
                        continue;
                    }
                    if let Some(why) = forbidden_property(&lower) {
                        self.add(line, format!("{name} can't be set."), Some(why));
                        skip_declaration(parser);
                        continue;
                    }
                    out.token(&Token::Ident(name.as_str().into()));
                    out.raw(":");
                    parser.skip_whitespace();
                    while let Ok(token) = parser.next_including_whitespace().cloned() {
                        if matches!(token, Token::Semicolon) {
                            break;
                        }
                        self.value_token(parser, out, &token, nesting);
                    }
                    out.raw(";");
                }
                token => {
                    self.add(
                        line,
                        format!(
                            "A declaration can't start with {}. A rule can't go inside a rule.",
                            token.to_css_string()
                        ),
                        None,
                    );
                    skip_declaration(parser);
                }
            }
        }
    }

    /// One token of a value or an at-rule's prelude, with what's nested in it.
    fn value_token<'i>(
        &mut self,
        parser: &mut Parser<'i>,
        out: &mut Writer,
        token: &Token<'i>,
        nesting: usize,
    ) {
        let line = line_of(parser);
        let close = match token {
            Token::Function(name) => {
                if forbidden_function(name) {
                    self.add(
                        line,
                        format!("{name}() can't be used. Custom pages load nothing from other sites, so customers on Tor stay private."),
                        Some("Use a colour or gradient instead."),
                    );
                    return;
                }
                ")"
            }
            Token::ParenthesisBlock => ")",
            Token::SquareBracketBlock => "]",
            // To a browser, `{}` in a value can be a rule inside the rule,
            // with a selector and properties nothing here has looked at.
            Token::CurlyBracketBlock => {
                self.add(
                    line,
                    "{ } can't go inside a value. A rule can't go inside a rule.".to_string(),
                    None,
                );
                return;
            }
            Token::Delim('\\') => {
                self.add(line, "A stray \\ can't be used.".to_string(), None);
                return;
            }
            Token::Delim('&') => {
                self.add(
                    line,
                    "& can't be used: a rule can't go inside a rule.".to_string(),
                    None,
                );
                return;
            }
            Token::AtKeyword(name) => {
                self.add(line, format!("@{name} can't be in a value."), None);
                return;
            }
            Token::UnquotedUrl(_) => {
                self.add(
                    line,
                    "url() can't be used. Custom pages load nothing from other sites, so customers on Tor stay private.".to_string(),
                    Some("Use a colour or gradient instead."),
                );
                return;
            }
            Token::BadUrl(_) | Token::BadString(_) => {
                self.add(line, "This value can't be read.".to_string(), None);
                return;
            }
            Token::CloseParenthesis | Token::CloseSquareBracket | Token::CloseCurlyBracket => {
                self.add(
                    line,
                    "A closing bracket here has no opening one.".to_string(),
                    None,
                );
                return;
            }
            token => {
                out.token(token);
                return;
            }
        };
        if nesting >= MAX_NESTING {
            self.add(line, "This CSS nests too deeply.".to_string(), None);
            return;
        }
        out.token(token);
        let _: Result<(), cssparser::ParseError<()>> = parser.parse_nested_block(|parser| {
            while let Ok(inner) = parser.next_including_whitespace().cloned() {
                self.value_token(parser, out, &inner, nesting + 1);
            }
            Ok(())
        });
        out.raw(close);
    }
}

/// Moves past the rest of a rule that won't be written.
fn skip_rule(parser: &mut Parser) {
    while let Ok(token) = parser.next() {
        if matches!(token, Token::Semicolon | Token::CurlyBracketBlock) {
            return;
        }
    }
}

/// Moves past the rest of a declaration that won't be written.
fn skip_declaration(parser: &mut Parser) {
    while let Ok(token) = parser.next() {
        if matches!(token, Token::Semicolon) {
            return;
        }
    }
}

/// Whether what [`sanitize`] wrote reads, as a browser would read it, as rules
/// that stay inside the page and load nothing: every selector starts at the
/// scope and goes down from it, only allowed at-rules, nothing nested in a
/// rule's values. It knows nothing of how the text was written, so a mistake
/// in writing it is caught here rather than reaching a customer's browser.
pub fn written_is_safe(written: &str) -> bool {
    let mut parser = Parser::new(written);
    rules_safe(&mut parser, 0)
}

fn rules_safe(parser: &mut Parser, depth: usize) -> bool {
    if depth > MAX_NESTING + 1 {
        return false;
    }
    while let Ok(token) = parser.next_including_whitespace().cloned() {
        let safe = match token {
            Token::WhiteSpace(_) => true,
            Token::AtKeyword(ref name) => {
                let name = name.to_ascii_lowercase();
                if name == "keyframes" {
                    parser.expect_ident().is_ok()
                        && parser.expect_curly_bracket_block().is_ok()
                        && nested(parser, |parser| keyframes_safe(parser, depth + 1))
                } else if GROUP_AT_RULES.contains(&name.as_str()) {
                    prelude_safe(parser, depth)
                        && nested(parser, |parser| rules_safe(parser, depth + 1))
                } else {
                    false
                }
            }
            first => style_rule_safe(parser, first, depth),
        };
        if !safe {
            return false;
        }
    }
    true
}

/// Up to and into the `{`, with nothing in the way that isn't plain values.
fn prelude_safe(parser: &mut Parser, depth: usize) -> bool {
    while let Ok(token) = parser.next_including_whitespace().cloned() {
        match token {
            Token::CurlyBracketBlock => return true,
            Token::Semicolon => return false,
            token => {
                if !value_safe(parser, &token, depth) {
                    return false;
                }
            }
        }
    }
    false
}

fn keyframes_safe(parser: &mut Parser, depth: usize) -> bool {
    while let Ok(token) = parser.next().cloned() {
        match token {
            Token::Percentage { .. } | Token::Comma | Token::Ident(_) => {}
            Token::CurlyBracketBlock => {
                if !nested(parser, |parser| declarations_safe(parser, depth)) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

/// A rule from its first token: each selector starts `#checkout-root `, then
/// goes down, never sideways.
fn style_rule_safe<'i>(parser: &mut Parser<'i>, first: Token<'i>, depth: usize) -> bool {
    let mut token = first;
    loop {
        if !matches!(&token, Token::IDHash(id) if id.as_ref() == "checkout-root")
            || !matches!(parser.next_including_whitespace(), Ok(Token::WhiteSpace(_)))
        {
            return false;
        }
        let mut at_start = true;
        loop {
            let Ok(next) = parser.next_including_whitespace().cloned() else {
                return false;
            };
            match next {
                Token::CurlyBracketBlock => {
                    return !at_start && nested(parser, |parser| declarations_safe(parser, depth));
                }
                Token::Comma => break,
                Token::Delim('~' | '+' | '>' | '|') if at_start => return false,
                Token::Delim('&') | Token::Semicolon => return false,
                Token::WhiteSpace(_) => continue,
                next => {
                    if !value_safe(parser, &next, depth) {
                        return false;
                    }
                }
            }
            at_start = false;
        }
        let Ok(next) = parser.next_including_whitespace().cloned() else {
            return false;
        };
        token = next;
    }
}

fn declarations_safe(parser: &mut Parser, depth: usize) -> bool {
    while let Ok(token) = parser.next_including_whitespace().cloned() {
        if !value_safe(parser, &token, depth) {
            return false;
        }
    }
    true
}

/// A token anywhere in a selector or a value, with what's nested in it.
fn value_safe<'i>(parser: &mut Parser<'i>, token: &Token<'i>, depth: usize) -> bool {
    if depth > MAX_NESTING + 1 {
        return false;
    }
    match token {
        Token::CurlyBracketBlock
        | Token::UnquotedUrl(_)
        | Token::BadUrl(_)
        | Token::BadString(_)
        | Token::AtKeyword(_)
        | Token::Delim('\\' | '&')
        | Token::CloseParenthesis
        | Token::CloseSquareBracket
        | Token::CloseCurlyBracket => false,
        Token::Function(name) if forbidden_function(name) => false,
        Token::Function(_) | Token::ParenthesisBlock | Token::SquareBracketBlock => {
            nested(parser, |parser| {
                while let Ok(inner) = parser.next_including_whitespace().cloned() {
                    if !value_safe(parser, &inner, depth + 1) {
                        return false;
                    }
                }
                true
            })
        }
        _ => true,
    }
}

/// Runs `check` on the block just read.
fn nested<'i>(parser: &mut Parser<'i>, check: impl FnOnce(&mut Parser<'i>) -> bool) -> bool {
    let result: Result<bool, cssparser::ParseError<()>> =
        parser.parse_nested_block(|parser| Ok(check(parser)));
    result.unwrap_or(false)
}
