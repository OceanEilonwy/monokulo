//! Reads a template's strict HTML and its tokens into a tree.
//!
//! Far stricter than a browser, on purpose. Everything a browser would quietly
//! rearrange (a block in a paragraph, an unclosed tag, a table) is refused, so
//! the tree this builds is the tree a browser builds from what's written back.
//! It works from an explicit stack, so a hostile template can't overflow the
//! real one.

use std::collections::BTreeSet;

use crate::feature::{Feature, Part};
use crate::state::Status;
use crate::{
    Attribute, Clause, Flag, Node, Problem, Template, MAX_DEPTH, MAX_PROBLEMS, MAX_SOURCE_BYTES,
    MAX_TAGS,
};

/// Elements a template may use. No links or forms: nothing that goes anywhere.
/// No tables, whose parsing rules move content around. No images, media,
/// scripts, styles or frames.
const BLOCK_TAGS: [&str; 15] = [
    "article",
    "aside",
    "blockquote",
    "div",
    "figcaption",
    "figure",
    "footer",
    "header",
    "hr",
    "li",
    "main",
    "ol",
    "section",
    "ul",
    "nav",
];
/// Elements that hold only phrasing content: a block inside one is what a
/// browser would move out.
const PHRASING_ONLY_TAGS: [&str; 7] = ["p", "h1", "h2", "h3", "h4", "h5", "h6"];
const INLINE_TAGS: [&str; 8] = ["b", "br", "code", "em", "i", "small", "span", "strong"];
const ATTRIBUTES: [&str; 3] = ["class", "lang", "aria-label"];

pub(crate) fn is_void(tag: &str) -> bool {
    matches!(tag, "br" | "hr")
}

fn known_tag(name: &str) -> Option<&'static str> {
    BLOCK_TAGS
        .iter()
        .chain(&PHRASING_ONLY_TAGS)
        .chain(&INLINE_TAGS)
        .find(|tag| **tag == name)
        .copied()
}

fn is_inline_tag(tag: &str) -> bool {
    INLINE_TAGS.contains(&tag)
}

/// Whether its contents must be phrasing content.
fn holds_only_phrasing(tag: &str) -> bool {
    PHRASING_ONLY_TAGS.contains(&tag) || is_inline_tag(tag)
}

enum Open {
    Root,
    Element {
        tag: &'static str,
        attributes: Vec<Attribute>,
    },
    Region {
        id: usize,
        clauses: Vec<Clause>,
        source: String,
    },
}

struct Frame {
    open: Open,
    line: u32,
    children: Vec<Node>,
}

struct Parser<'a> {
    source: &'a str,
    pos: usize,
    line: u32,
    stack: Vec<Frame>,
    problems: Vec<Problem>,
    regions: usize,
    classes: BTreeSet<String>,
    size: u32,
    /// Tags refused when they opened, so their closing tags aren't a second
    /// problem for the merchant to wade through.
    refused: Vec<String>,
}

/// Parses a template, or says everything found wrong with it.
pub(crate) fn parse(source: &str) -> Result<Template, Vec<Problem>> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(vec![Problem::at(
            1,
            format!(
                "This template is over the limit of {} KiB.",
                MAX_SOURCE_BYTES / 1024
            ),
        )]);
    }
    if source.matches('<').count() > MAX_TAGS {
        return Err(vec![Problem::at(
            1,
            format!("This template has more than {MAX_TAGS} tags."),
        )]);
    }
    let source = crate::normalize(source);
    let mut parser = Parser {
        source: &source,
        pos: 0,
        line: 1,
        stack: vec![Frame {
            open: Open::Root,
            line: 1,
            children: Vec::new(),
        }],
        problems: Vec::new(),
        regions: 0,
        classes: BTreeSet::new(),
        size: 0,
        refused: Vec::new(),
    };
    parser.run();
    if !parser.problems.is_empty() {
        parser.problems.truncate(MAX_PROBLEMS);
        return Err(parser.problems);
    }
    let root = parser.stack.pop().expect("the root is never closed");
    Ok(Template {
        nodes: root.children,
        regions: parser.regions,
        classes: parser.classes,
        size: parser.size,
    })
}

impl<'a> Parser<'a> {
    fn rest(&self) -> &'a str {
        &self.source[self.pos..]
    }

    /// Moves on by `len` bytes, keeping count of lines.
    fn advance(&mut self, len: usize) {
        let taken = &self.source[self.pos..self.pos + len];
        self.line += taken.matches('\n').count() as u32;
        self.pos += len;
    }

    fn problem(&mut self, problem: Problem) {
        self.problems.push(problem);
    }

    fn run(&mut self) {
        while self.pos < self.source.len() && self.problems.len() < MAX_PROBLEMS {
            let rest = self.rest();
            if rest.starts_with("<!--") {
                self.comment();
            } else if rest.starts_with("</") {
                self.close_tag();
            } else if rest.starts_with('<') {
                self.open_tag();
            } else if rest.starts_with("{{") {
                self.token();
            } else {
                self.text();
            }
        }
        // Whatever is still open was never closed.
        while self.stack.len() > 1 && self.problems.len() < MAX_PROBLEMS {
            let frame = self.stack.last().expect("checked above");
            let line = frame.line;
            let problem = match &frame.open {
                Open::Element { tag, .. } => Problem::at(line, format!("<{tag}> is never closed."))
                    .hint(format!("Add </{tag}> where it ends.")),
                Open::Region { .. } => Problem::at(line, "This {{if}} has no {{end}}."),
                Open::Root => unreachable!("the root is the stack's first frame"),
            };
            self.problem(problem);
            self.stack.pop();
        }
    }

    fn comment(&mut self) {
        match self.rest().find("-->") {
            Some(end) => self.advance(end + 3),
            None => {
                let line = self.line;
                self.problem(Problem::at(line, "This comment is never closed."));
                self.pos = self.source.len();
            }
        }
    }

    /// Text up to the next tag or token.
    fn text(&mut self) {
        let rest = self.rest();
        let end = rest
            .find('<')
            .into_iter()
            .chain(rest.find("{{"))
            .min()
            .unwrap_or(rest.len());
        let raw = rest[..end].to_string();
        let line = self.line;
        self.advance(end);
        let text = match self.decode(&raw, line) {
            Some(text) => text,
            None => return,
        };
        if !text.trim_ascii().is_empty() {
            self.check_text_allowed_here(line);
        }
        self.size += text.len().div_ceil(32) as u32;
        self.stack
            .last_mut()
            .expect("the root is never closed")
            .children
            .push(Node::Text(text));
    }

    /// The nearest element around where we are, looking through regions, which
    /// aren't elements to a reader.
    fn parent_tag(&self) -> Option<&'static str> {
        self.stack.iter().rev().find_map(|frame| match frame.open {
            Open::Element { tag, .. } => Some(tag),
            _ => None,
        })
    }

    fn inside_phrasing_only(&self) -> Option<&'static str> {
        self.stack.iter().rev().find_map(|frame| match frame.open {
            Open::Element { tag, .. } if holds_only_phrasing(tag) => Some(tag),
            _ => None,
        })
    }

    fn check_text_allowed_here(&mut self, line: u32) {
        if let Some(tag @ ("ul" | "ol")) = self.parent_tag() {
            self.problem(
                Problem::at(line, format!("Text can't go directly inside <{tag}>."))
                    .hint("Put it in an <li>."),
            );
        }
    }

    /// Decodes the few character references a template may use. `None` after
    /// recording a problem.
    fn decode(&mut self, raw: &str, line: u32) -> Option<String> {
        let mut out = String::with_capacity(raw.len());
        let mut rest = raw;
        loop {
            let Some(at) = rest.find('&') else {
                out.push_str(rest);
                break;
            };
            out.push_str(&rest[..at]);
            rest = &rest[at..];
            let reference = rest[1..]
                .find(';')
                .filter(|end| *end <= 10)
                .map(|end| &rest[1..end + 1]);
            let decoded = reference.and_then(decode_reference);
            match (reference, decoded) {
                (Some(reference), Some(c)) => {
                    out.push(c);
                    rest = &rest[reference.len() + 2..];
                }
                _ => {
                    self.problem(
                        Problem::at(line, "This & doesn't start a character this page can use.")
                            .hint("Write &amp; for an ampersand."),
                    );
                    return None;
                }
            }
        }
        if let Some(c) = out.chars().find(|c| !allowed_char(*c)) {
            self.problem(Problem::at(
                line,
                format!("Character U+{:04X} can't be used.", u32::from(c)),
            ));
            return None;
        }
        Some(out)
    }

    fn open_tag(&mut self) {
        let line = self.line;
        let rest = self.rest();
        let name_len = rest[1..]
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '-')
            .unwrap_or(rest.len() - 1);
        let name = rest[1..1 + name_len].to_ascii_lowercase();
        if name.is_empty() {
            self.problem(
                Problem::at(line, "This < doesn't start a tag.").hint("Write &lt; for a <."),
            );
            self.advance(1);
            return;
        }
        self.advance(1 + name_len);
        let attributes = self.attributes();
        let self_closed = self.finish_tag(line);

        let Some(tag) = known_tag(&name) else {
            let problem = Problem::at(line, format!("<{name}> can't be used on a checkout page."));
            let hint = match name.as_str() {
                "a" => "Links aren't available: a checkout has nowhere else to send a customer.",
                "script" | "style" | "link" | "meta" | "iframe" | "object" | "embed" | "base" => {
                    "Custom pages load nothing and run nothing of their own. Put styling in the CSS box."
                }
                "img" | "svg" | "picture" | "video" | "audio" | "source" => {
                    "Custom pages load nothing from other sites, so customers on Tor stay private."
                }
                "table" | "thead" | "tbody" | "tr" | "td" | "th" => {
                    "Use <div> with class=\"mk-row\" and CSS for layout."
                }
                "form" | "input" | "button" | "select" | "textarea" => {
                    "Use {{refund}} for the refund address."
                }
                _ => "See the custom page guide for what a template can use.",
            };
            self.problem(problem.hint(hint));
            self.refused.push(name);
            return;
        };
        if self_closed && !is_void(tag) {
            self.problem(Problem::at(
                line,
                format!("<{tag}/> can't close itself: write <{tag}></{tag}>."),
            ));
            return;
        }
        if let Some(outer) = self.inside_phrasing_only().filter(|_| !is_inline_tag(tag)) {
            self.problem(
                Problem::at(
                    line,
                    format!("<{tag}> can't go inside <{outer}>. Browsers would break it apart."),
                )
                .hint(format!("Move it out of the <{outer}>.")),
            );
            self.refused.push(name);
            return;
        }
        match (tag, self.parent_tag()) {
            ("li", Some("ul" | "ol")) | (_, _) if tag != "li" && !self.in_list() => {}
            ("li", Some("ul" | "ol")) => {}
            ("li", _) => {
                self.problem(Problem::at(
                    line,
                    "<li> must be directly inside <ul> or <ol>.",
                ));
                return;
            }
            _ => {
                self.problem(Problem::at(
                    line,
                    format!("<{tag}> can't go directly inside a list: put it in an <li>."),
                ));
                return;
            }
        }
        // Each element costs a little whatever is in it.
        self.size += 1;
        for attribute in &attributes {
            self.size += attribute.value.len().div_ceil(32) as u32;
            if attribute.name == "class" {
                self.classes
                    .extend(attribute.value.split_ascii_whitespace().map(str::to_string));
            }
        }
        if is_void(tag) {
            self.stack
                .last_mut()
                .expect("the root is never closed")
                .children
                .push(Node::Element {
                    tag,
                    attributes,
                    children: Vec::new(),
                });
            return;
        }
        if self.element_depth() >= MAX_DEPTH {
            self.problem(Problem::at(
                line,
                format!("Elements can only nest {MAX_DEPTH} deep."),
            ));
            return;
        }
        self.stack.push(Frame {
            open: Open::Element { tag, attributes },
            line,
            children: Vec::new(),
        });
    }

    fn in_list(&self) -> bool {
        matches!(self.parent_tag(), Some("ul" | "ol"))
    }

    fn element_depth(&self) -> usize {
        self.stack
            .iter()
            .filter(|frame| matches!(frame.open, Open::Element { .. }))
            .count()
    }

    /// Reads a tag's attributes, up to its `>` or `/>`.
    fn attributes(&mut self) -> Vec<Attribute> {
        let mut attributes: Vec<Attribute> = Vec::new();
        loop {
            self.skip_whitespace();
            let rest = self.rest();
            if rest.is_empty() || rest.starts_with('>') || rest.starts_with("/>") {
                return attributes;
            }
            let line = self.line;
            let name_len = rest
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '-')
                .unwrap_or(rest.len());
            if name_len == 0 {
                self.problem(Problem::at(
                    line,
                    "This tag has something it can't read in it.",
                ));
                // On to the end of the tag, whatever is in it.
                let end = rest.find('>').unwrap_or(rest.len());
                self.advance(end);
                return attributes;
            }
            let name = rest[..name_len].to_ascii_lowercase();
            self.advance(name_len);
            let mut value = None;
            if self.rest().starts_with('=') {
                self.advance(1);
                value = self.attribute_value(line, &name);
            }
            let Some(known) = ATTRIBUTES.iter().find(|known| **known == name) else {
                let hint = match name.as_str() {
                    "style" => "Put styling in the CSS box.",
                    "id" => "Use class: ids are the page's own.",
                    n if n.starts_with("on") => "Custom pages run no scripts of their own.",
                    _ => "A template can use class, lang and aria-label.",
                };
                self.problem(
                    Problem::at(line, format!("The {name} attribute can't be used.")).hint(hint),
                );
                continue;
            };
            let Some(value) = value else {
                self.problem(Problem::at(
                    line,
                    format!("{name} needs a value in quotes."),
                ));
                continue;
            };
            if attributes.iter().any(|attribute| attribute.name == *known) {
                self.problem(Problem::at(line, format!("{name} is on this tag twice.")));
                continue;
            }
            if *known == "class"
                && !value.split_ascii_whitespace().all(|class| {
                    class
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                })
            {
                self.problem(Problem::at(
                    line,
                    "A class can only use letters, digits, hyphens and underscores.",
                ));
                continue;
            }
            attributes.push(Attribute { name: known, value });
        }
    }

    fn attribute_value(&mut self, line: u32, name: &str) -> Option<String> {
        let rest = self.rest();
        let Some(quote) = rest.chars().next().filter(|c| matches!(c, '"' | '\'')) else {
            self.problem(Problem::at(line, format!("{name}'s value needs quotes.")));
            let end = rest
                .find(|c: char| c.is_ascii_whitespace() || c == '>')
                .unwrap_or(rest.len());
            self.advance(end);
            return None;
        };
        let Some(end) = rest[1..].find(quote) else {
            self.problem(Problem::at(
                line,
                format!("{name}'s value is never closed."),
            ));
            self.pos = self.source.len();
            return None;
        };
        let raw = rest[1..1 + end].to_string();
        self.advance(end + 2);
        let decoded = self.decode(&raw, line)?;
        if decoded.contains("{{") {
            self.problem(
                Problem::at(line, "A {{...}} can't go inside an attribute.")
                    .hint("Put it between tags."),
            );
            return None;
        }
        Some(decoded)
    }

    fn skip_whitespace(&mut self) {
        let len = self
            .rest()
            .find(|c: char| !c.is_ascii_whitespace())
            .unwrap_or(self.rest().len());
        self.advance(len);
    }

    /// Reads the `>` or `/>` ending a tag: whether it was `/>`.
    fn finish_tag(&mut self, line: u32) -> bool {
        self.skip_whitespace();
        if self.rest().starts_with("/>") {
            self.advance(2);
            true
        } else if self.rest().starts_with('>') {
            self.advance(1);
            false
        } else {
            self.problem(Problem::at(line, "This tag is never closed with a >."));
            self.pos = self.source.len();
            false
        }
    }

    fn close_tag(&mut self) {
        let line = self.line;
        let rest = &self.rest()[2..];
        let name_len = rest
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '-')
            .unwrap_or(rest.len());
        let name = rest[..name_len].to_ascii_lowercase();
        self.advance(2 + name_len);
        self.skip_whitespace();
        if self.rest().starts_with('>') {
            self.advance(1);
        } else {
            self.problem(Problem::at(
                line,
                format!("</{name}> is never closed with a >."),
            ));
            return;
        }
        if let Some(at) = self.refused.iter().position(|refused| *refused == name) {
            self.refused.swap_remove(at);
            return;
        }
        match self.stack.last().map(|frame| &frame.open) {
            Some(Open::Element { tag, .. }) if *tag == name => self.close_frame(),
            Some(Open::Region { .. }) => self.problem(
                Problem::at(
                    line,
                    format!("</{name}> is inside an {{{{if}}}} that started outside it."),
                )
                .hint("Each {{if}} and its {{end}} must be inside the same element."),
            ),
            Some(Open::Element { tag, .. }) => {
                let open = *tag;
                let opened = self.stack.last().map_or(line, |frame| frame.line);
                self.problem(Problem::at(
                    line,
                    format!("</{name}> doesn't match <{open}> on line {opened}."),
                ));
                // The tag it does match, if any, closes what's inside it: fewer
                // knock-on problems for the merchant to wade through.
                if self
                    .stack
                    .iter()
                    .any(|frame| matches!(frame.open, Open::Element { tag, .. } if tag == name))
                {
                    while !matches!(self.stack.last().map(|frame| &frame.open), Some(Open::Element { tag, .. }) if *tag == name)
                    {
                        self.close_frame();
                    }
                    self.close_frame();
                }
            }
            _ => self.problem(Problem::at(line, format!("</{name}> has no opening tag."))),
        }
    }

    /// Closes the innermost open element or region, into what holds it.
    fn close_frame(&mut self) {
        let frame = self.stack.pop().expect("only called with something open");
        let node = match frame.open {
            Open::Element { tag, attributes } => Node::Element {
                tag,
                attributes,
                children: frame.children,
            },
            Open::Region {
                id,
                clauses,
                source,
            } => Node::Region {
                id,
                clauses,
                source,
                children: frame.children,
            },
            Open::Root => unreachable!("the root is never closed"),
        };
        self.stack
            .last_mut()
            .expect("the root is never closed")
            .children
            .push(node);
    }

    /// A `{{...}}`.
    fn token(&mut self) {
        let line = self.line;
        let rest = self.rest();
        let Some(end) = rest.find("}}") else {
            self.problem(Problem::at(line, "This {{ is never closed with }}."));
            self.pos = self.source.len();
            return;
        };
        let raw = &rest[2..end];
        let on_one_line = !raw.contains('\n');
        let body = raw.trim().to_string();
        self.advance(end + 2);
        if !on_one_line {
            self.problem(Problem::at(line, "A {{...}} must be on one line."));
            return;
        }
        let (word, args) = match body.split_once(char::is_whitespace) {
            Some((word, args)) => (word, args.trim()),
            None => (body.as_str(), ""),
        };
        match word {
            "if" => self.open_region(line, &body, args),
            "end" => self.end_region(line),
            "else" | "elif" => self.problem(
                Problem::at(line, format!("{{{{{word}}}}} isn't available."))
                    .hint("Use a second {{if}} with the opposite condition."),
            ),
            word => self.part(line, word, args),
        }
    }

    fn part(&mut self, line: u32, word: &str, args: &str) {
        let Some(feature) = Feature::from_name(word) else {
            self.problem(
                Problem::at(
                    line,
                    format!("{{{{{word}}}}} isn't something a page can show."),
                )
                .hint("See the custom page guide for what a template can use."),
            );
            return;
        };
        let mut option = None;
        for arg in args.split(',').map(str::trim).filter(|arg| !arg.is_empty()) {
            match feature.options().iter().find(|known| **known == arg) {
                Some(known) if option.is_none() => option = Some(*known),
                Some(_) => self.problem(Problem::at(
                    line,
                    format!("{{{{{word}}}}} can take only one option."),
                )),
                None => {
                    let hint = match feature.options() {
                        [] => "This feature takes no options.".to_string(),
                        options => format!("It can be: {}.", options.join(", ")),
                    };
                    self.problem(
                        Problem::at(
                            line,
                            format!("{{{{{word} {args}}}}}: \"{arg}\" isn't an option."),
                        )
                        .hint(hint),
                    );
                }
            }
        }
        // A region is replaced when it flips, which would lose what's being
        // typed in the form, and the script that drives it.
        if feature == Feature::Refund
            && self
                .stack
                .iter()
                .any(|frame| matches!(frame.open, Open::Region { .. }))
        {
            self.problem(
                Problem::at(line, "{{refund}} can't go inside an {{if}}.")
                    .hint("It says by itself when the address can't change any more."),
            );
            return;
        }
        if !feature.is_inline() {
            if let Some(outer) = self.inside_phrasing_only() {
                self.problem(
                    Problem::at(line, format!("{{{{{word}}}}} can't go inside <{outer}>. Browsers would break it apart."))
                        .hint(format!("Move it out of the <{outer}>.")),
                );
                return;
            }
        }
        if self.in_list() {
            self.problem(Problem::at(
                line,
                format!("{{{{{word}}}}} can't go directly inside a list: put it in an <li>."),
            ));
            return;
        }
        self.size += feature.cost();
        self.stack
            .last_mut()
            .expect("the root is never closed")
            .children
            .push(Node::Part {
                part: Part { feature, option },
                line,
            });
    }

    fn open_region(&mut self, line: u32, source: &str, args: &str) {
        if self
            .stack
            .iter()
            .any(|frame| matches!(frame.open, Open::Region { .. }))
        {
            self.problem(
                Problem::at(line, "An {{if}} can't go inside another {{if}}.")
                    .hint("Put everything it asks in one {{if}}: {{if awaiting, !fiat}}."),
            );
            return;
        }
        let mut clauses = Vec::new();
        for clause in args.split(',').map(str::trim) {
            match parse_clause(clause) {
                Ok(clause) => clauses.push(clause),
                Err(problem) => self.problem(Problem::at(line, problem.0).hint(problem.1)),
            }
        }
        // 3, and 1 per thing it asks: what evaluating it costs each render.
        let asks: usize = clauses
            .iter()
            .map(|clause| match clause {
                Clause::Status(set) => set.len(),
                Clause::Flag { .. } => 1,
            })
            .sum();
        self.size += 3 + asks as u32;
        let id = self.regions;
        self.regions += 1;
        self.stack.push(Frame {
            open: Open::Region {
                id,
                clauses,
                source: source.to_string(),
            },
            line,
            children: Vec::new(),
        });
    }

    fn end_region(&mut self, line: u32) {
        match self.stack.last().map(|frame| &frame.open) {
            Some(Open::Region { .. }) => self.close_frame(),
            Some(Open::Element { tag, .. }) => {
                let tag = *tag;
                self.problem(
                    Problem::at(
                        line,
                        format!(
                            "This {{{{end}}}} is inside a <{tag}> its {{{{if}}}} is outside of."
                        ),
                    )
                    .hint("Each {{if}} and its {{end}} must be inside the same element."),
                );
            }
            _ => self.problem(Problem::at(line, "This {{end}} has no {{if}}.")),
        }
    }
}

fn parse_clause(clause: &str) -> Result<Clause, (String, String)> {
    const VARIABLES: &str =
        "It can ask: awaiting, final, double_spend, fiat, refund_locked or status=paid|overpaid.";
    if clause.is_empty() {
        return Err((
            "This {{if}} doesn't ask anything.".to_string(),
            VARIABLES.to_string(),
        ));
    }
    if let Some(statuses) = clause.strip_prefix("status=") {
        let mut set = Vec::new();
        for name in statuses.split('|') {
            match Status::from_name(name.trim()) {
                Some(status) => set.push(status),
                None => {
                    let known: Vec<&str> = Status::ALL.iter().map(|s| s.name()).collect();
                    return Err((
                        format!("\"{name}\" isn't a status."),
                        format!("A status can be: {}.", known.join(", ")),
                    ));
                }
            }
        }
        return Ok(Clause::Status(set));
    }
    let (negated, name) = match clause.strip_prefix('!') {
        Some(name) => (true, name.trim()),
        None => (false, clause),
    };
    let flag = match name {
        "awaiting" => Flag::Awaiting,
        "final" => Flag::Final,
        "double_spend" => Flag::DoubleSpend,
        "fiat" => Flag::Fiat,
        "refund_locked" => Flag::RefundLocked,
        name => {
            return Err((
                format!("\"{name}\" isn't something an {{{{if}}}} can ask."),
                VARIABLES.to_string(),
            ))
        }
    };
    Ok(Clause::Flag { flag, negated })
}

fn decode_reference(reference: &str) -> Option<char> {
    match reference {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{a0}'),
        _ => {
            let number = reference.strip_prefix('#')?;
            let value = match number.strip_prefix(['x', 'X']) {
                Some(hex) if hex.bytes().all(|b| b.is_ascii_hexdigit()) => {
                    u32::from_str_radix(hex, 16).ok()?
                }
                Some(_) => return None,
                None if number.bytes().all(|b| b.is_ascii_digit()) => number.parse().ok()?,
                None => return None,
            };
            char::from_u32(value).filter(|c| allowed_char(*c))
        }
    }
}

/// Characters a browser reads as written: no controls, whose numeric
/// references browsers remap, and no noncharacters.
fn allowed_char(c: char) -> bool {
    let code = u32::from(c);
    match code {
        0x09 | 0x0a => true,
        0..=0x1f | 0x7f..=0x9f => false,
        0xfdd0..=0xfdef => false,
        _ => code & 0xfffe != 0xfffe,
    }
}
