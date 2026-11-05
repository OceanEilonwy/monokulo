//! Checkout page templates: `{{feature arg}}` tokens and `{{if ...}}` regions
//! in a small, strict subset of HTML, with optional CSS.
//!
//! A template is untrusted text that will run on Monokulo's origin, on a page
//! people pay from, so nothing in it reaches a browser as written. It's parsed
//! to a tree and the page is written from that tree ([`Template::render`]); its
//! CSS is read token by token and written back scoped ([`css::sanitize`]). What
//! this crate writes parses to the same tree in a browser, which rules out the
//! differences between parsers that sanitisers are usually attacked through:
//! see the corpus test in `e2e/browser/tests/checkout-template-corpus.spec.js`.
//!
//! Saving runs the cheap checks first, so a large or hostile template costs
//! little: [`MAX_SOURCE_BYTES`], [`MAX_TAGS`], parsing with [`MAX_DEPTH`],
//! [`BUDGET`], then every order [`State`] ([`check`]). A published template
//! has to parse and show a customer what they need in every state, but not fit
//! the budget ([`check_published`]): a budget may tighten in a later release, and
//! that mustn't take down a page that was accepted, as a closed hole should.
//!
//! This crate knows nothing of what a feature renders: its caller passes that
//! in ([`Template::render`]).

pub mod css;
mod feature;
mod parse;
mod state;

use std::collections::BTreeSet;
use std::fmt::Write as _;

pub use feature::{Feature, Part, ALL_FEATURES};
pub use state::{all_states, State, Status};

/// The largest template source accepted, in bytes, before it is even read.
pub const MAX_SOURCE_BYTES: usize = 32 * 1024;
/// The most tags a template source may have, counted before parsing.
pub const MAX_TAGS: usize = 2000;
/// How deeply elements may nest.
pub const MAX_DEPTH: usize = 32;
/// The most a template may cost ([`Template::size`]): shown to the merchant as
/// its size. Not a setting: it keeps what one page view costs small, and that
/// is the same on every instance.
pub const BUDGET: u32 = 4000;
/// The most problems reported for one template.
pub(crate) const MAX_PROBLEMS: usize = 20;

/// Where a [`Problem`] is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// A line of the template, from 1.
    Template(u32),
    /// A line of the CSS, from 1.
    Css(u32),
    /// The template as a whole, in some order state.
    EveryState,
}

/// One thing wrong with a template or its CSS, in the merchant's terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub place: Place,
    pub message: String,
    /// What to do about it.
    pub hint: Option<String>,
}

impl Problem {
    pub(crate) fn at(line: u32, message: impl Into<String>) -> Self {
        Problem {
            place: Place::Template(line),
            message: message.into(),
            hint: None,
        }
    }

    pub(crate) fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// What a condition asks. A condition is a list of these, all of which must
/// hold: no `or`, `else` or nesting, so what shows when is plain to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Clause {
    Flag { flag: Flag, negated: bool },
    Status(Vec<Status>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flag {
    Awaiting,
    Final,
    DoubleSpend,
    Fiat,
    RefundLocked,
}

impl Clause {
    fn holds(&self, state: &State) -> bool {
        match self {
            Clause::Flag { flag, negated } => {
                let value = match flag {
                    Flag::Awaiting => state.awaiting(),
                    Flag::Final => state.is_final(),
                    Flag::DoubleSpend => state.double_spend,
                    Flag::Fiat => state.fiat,
                    Flag::RefundLocked => state.refund_locked,
                };
                value != *negated
            }
            Clause::Status(set) => set.contains(&state.status),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Attribute {
    pub name: &'static str,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Node {
    Text(String),
    Element {
        tag: &'static str,
        attributes: Vec<Attribute>,
        children: Vec<Node>,
    },
    Part {
        part: Part,
        line: u32,
    },
    Region {
        id: usize,
        clauses: Vec<Clause>,
        source: String,
        children: Vec<Node>,
    },
}

/// A parsed template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    nodes: Vec<Node>,
    regions: usize,
    classes: BTreeSet<String>,
    size: u32,
}

/// Parses `source` and checks everything a template being saved must meet.
pub fn check(source: &str) -> Result<Template, Vec<Problem>> {
    let template = Template::load(source)?;
    let mut problems = Vec::new();
    if template.size > BUDGET {
        problems.push(Problem {
            place: Place::EveryState,
            message: format!(
                "This template is {} of {}.",
                format_size(template.size),
                format_size(BUDGET),
            ),
            hint: Some("Remove some text or conditions.".to_string()),
        });
    }
    problems.extend(template.every_state_problems());
    if problems.is_empty() {
        Ok(template)
    } else {
        Err(problems)
    }
}

/// Parses `source` and checks what a page already published must still meet: all
/// of [`check`] but its [`BUDGET`], which is about cost, not a customer's safety.
pub fn check_published(source: &str) -> Result<Template, Vec<Problem>> {
    let template = Template::load(source)?;
    let problems = template.every_state_problems();
    if problems.is_empty() {
        Ok(template)
    } else {
        Err(problems)
    }
}

/// A size as it's shown: `4,212`.
pub fn format_size(size: u32) -> String {
    let digits = size.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// The value that approves one version of a page: this store, this template
/// and this CSS. Any edit asks for a new one, and one store's can't be used by
/// another. Line endings don't count, as a browser's form sends CRLF.
pub fn version_token(store_id: &str, template: &str, css: &str) -> String {
    use sha2::Digest as _;
    let mut hash = sha2::Sha256::new();
    hash.update(b"monokulo-template-v1\0");
    for field in [store_id, &normalize(template), &normalize(css)] {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    base32(&hash.finalize()[..20])
}

/// Line endings as `\n`, as a template is stored and read.
pub fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let (mut out, mut buffer, mut bits) = (String::new(), 0u32, 0u32);
    for byte in bytes {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(ALPHABET[((buffer >> bits) & 31) as usize]));
        }
    }
    if bits > 0 {
        out.push(char::from(ALPHABET[((buffer << (5 - bits)) & 31) as usize]));
    }
    out
}

impl Template {
    /// Parses `source`, with only the checks that keep it safe to render: what
    /// loading a published template needs.
    pub fn load(source: &str) -> Result<Template, Vec<Problem>> {
        parse::parse(source)
    }

    /// What this template costs, against [`BUDGET`].
    pub fn size(&self) -> u32 {
        self.size
    }

    /// The classes this template uses, which its CSS may select.
    pub fn classes(&self) -> &BTreeSet<String> {
        &self.classes
    }

    /// How many `{{if}}` regions it has, numbered from 0.
    pub fn regions(&self) -> usize {
        self.regions
    }

    /// The page's body for `state`. `render_part` writes each feature that
    /// shows, as HTML this crate puts in place as it comes; every region is
    /// written, empty if its condition doesn't hold, so a live update has
    /// somewhere to put it.
    pub fn render(&self, state: &State, render_part: &mut dyn FnMut(&Part) -> String) -> String {
        let mut out = String::new();
        write_nodes(&self.nodes, state, render_part, &mut out);
        out
    }

    /// One region for `state`, as [`Template::render`] writes it, for a live
    /// update. `None` for a region that isn't this template's.
    pub fn render_region(
        &self,
        id: usize,
        state: &State,
        render_part: &mut dyn FnMut(&Part) -> String,
    ) -> Option<String> {
        fn find(nodes: &[Node], id: usize) -> Option<&Node> {
            nodes.iter().find_map(|node| match node {
                Node::Region { id: found, .. } if *found == id => Some(node),
                Node::Element { children, .. } => find(children, id),
                _ => None,
            })
        }
        let region = find(&self.nodes, id)?;
        let mut out = String::new();
        write_nodes(std::slice::from_ref(region), state, render_part, &mut out);
        Some(out)
    }

    /// Which regions' conditions hold for `state`, by id: what decides whether
    /// a live update must replace one.
    pub fn regions_showing(&self, state: &State) -> Vec<bool> {
        fn visit(nodes: &[Node], state: &State, out: &mut [bool]) {
            for node in nodes {
                match node {
                    Node::Region { id, clauses, .. } => {
                        out[*id] = clauses.iter().all(|clause| clause.holds(state));
                    }
                    Node::Element { children, .. } => visit(children, state, out),
                    _ => {}
                }
            }
        }
        let mut out = vec![false; self.regions];
        visit(&self.nodes, state, &mut out);
        out
    }

    /// The features that show in `state`.
    pub fn parts_showing(&self, state: &State) -> Vec<Part> {
        let mut out = Vec::new();
        visit_showing(&self.nodes, state, &mut |part, _| out.push(*part));
        out
    }

    /// What must hold in every state a page can be in: the customer can always
    /// see where their order stands, and while they owe money, how to pay and
    /// what. Each part with live updates shows once, as updates find it by id.
    fn every_state_problems(&self) -> Vec<Problem> {
        let mut problems: Vec<Problem> = Vec::new();
        let mut reported: BTreeSet<(Feature, bool)> = BTreeSet::new();
        for state in all_states() {
            let mut shown: Vec<(Part, u32)> = Vec::new();
            visit_showing(&self.nodes, &state, &mut |part, line| {
                shown.push((*part, line))
            });
            let shows = |feature: Feature| shown.iter().any(|(part, _)| part.feature == feature);

            let mut required = vec![Feature::Status];
            if state.awaiting() {
                required.extend([Feature::Qr, Feature::Amount, Feature::Address]);
                if state.fiat {
                    required.push(Feature::Fiat);
                }
            }
            for feature in required {
                if !shows(feature) && reported.insert((feature, false)) {
                    problems.push(Problem {
                        place: Place::EveryState,
                        message: format!(
                            "When {}, nothing shows {{{{{}}}}}, so {}.",
                            state.describe(),
                            feature.name(),
                            feature.why_required(),
                        ),
                        hint: self.only_shown_when(feature),
                    });
                }
            }
            for feature in ALL_FEATURES {
                if !feature.is_live() {
                    continue;
                }
                let lines: Vec<u32> = shown
                    .iter()
                    .filter(|(part, _)| part.feature == feature)
                    .map(|(_, line)| *line)
                    .collect();
                if lines.len() > 1 && reported.insert((feature, true)) {
                    problems.push(Problem {
                        place: Place::Template(lines[1]),
                        message: format!(
                            "{{{{{}}}}} shows twice when {}, on lines {} and {}. It can only show once at a time.",
                            feature.name(),
                            state.describe(),
                            lines[0],
                            lines[1],
                        ),
                        hint: None,
                    });
                }
            }
        }
        problems
    }

    /// Where a required feature does appear, if only under a condition.
    fn only_shown_when(&self, feature: Feature) -> Option<String> {
        fn find(nodes: &[Node], feature: Feature) -> Option<(u32, String)> {
            nodes.iter().find_map(|node| match node {
                Node::Element { children, .. } => find(children, feature),
                Node::Region {
                    source, children, ..
                } => children.iter().find_map(|child| match child {
                    Node::Part { part, line } if part.feature == feature => {
                        Some((*line, source.clone()))
                    }
                    Node::Element { children, .. } => {
                        find(children, feature).map(|(line, _)| (line, source.clone()))
                    }
                    _ => None,
                }),
                _ => None,
            })
        }
        find(&self.nodes, feature)
            .map(|(line, source)| format!("Line {line} only shows it when {{{{{source}}}}}."))
    }
}

fn visit_showing(nodes: &[Node], state: &State, visit: &mut dyn FnMut(&Part, u32)) {
    for node in nodes {
        match node {
            Node::Text(_) => {}
            Node::Part { part, line } => visit(part, *line),
            Node::Element { children, .. } => visit_showing(children, state, visit),
            Node::Region {
                clauses, children, ..
            } => {
                if clauses.iter().all(|clause| clause.holds(state)) {
                    visit_showing(children, state, visit);
                }
            }
        }
    }
}

fn write_nodes(
    nodes: &[Node],
    state: &State,
    render_part: &mut dyn FnMut(&Part) -> String,
    out: &mut String,
) {
    for node in nodes {
        match node {
            Node::Text(text) => write_escaped(text, false, out),
            Node::Part { part, .. } => out.push_str(&render_part(part)),
            Node::Element {
                tag,
                attributes,
                children,
            } => {
                let _ = write!(out, "<{tag}");
                for Attribute { name, value } in attributes {
                    let _ = write!(out, " {name}=\"");
                    write_escaped(value, true, out);
                    out.push('"');
                }
                out.push('>');
                if !parse::is_void(tag) {
                    write_nodes(children, state, render_part, out);
                    let _ = write!(out, "</{tag}>");
                }
            }
            Node::Region {
                id,
                clauses,
                children,
                ..
            } => {
                let _ = write!(out, "<mk-if id=\"mk-if-{id}\">");
                if clauses.iter().all(|clause| clause.holds(state)) {
                    write_nodes(children, state, render_part, out);
                }
                out.push_str("</mk-if>");
            }
        }
    }
}

fn write_escaped(text: &str, attribute: bool, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests;
