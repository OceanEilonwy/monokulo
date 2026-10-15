//! A template as data: what [`crate::component!`] parses each template
//! into. The interpreter renders it with a component's prop values; the
//! Solid translation writes it out as JSX.

/// `name` or `name.field`: a prop, or a name `@for` or `@if let` binds,
/// then a record's fields.
pub type Path = &'static [&'static str];

#[derive(Debug)]
pub enum Node {
    Element(Element),
    /// A string literal.
    Text(&'static str),
    /// `(name)`: text, a number, an enum's value, or a slot's content.
    Value(Path),
    /// `@if cond { } @else { }`
    If {
        cond: Cond,
        then: Vec<Node>,
        otherwise: Vec<Node>,
    },
    /// `@if let Some(bind) = path { } @else { }`
    IfLet {
        bind: &'static str,
        path: Path,
        then: Vec<Node>,
        otherwise: Vec<Node>,
    },
    /// `@for bind in path { }`
    For {
        bind: &'static str,
        path: Path,
        body: Vec<Node>,
    },
    /// `@if enhanced { } @else { }`: what only the behaviour renders, where
    /// the server leaves the marker `<!--mk:N-->`; and what the server
    /// renders in its place, between that marker and `<!--/mk:N-->`, which
    /// the behaviour takes away.
    Enhanced {
        mark: u32,
        body: Vec<Node>,
        fallback: Vec<Node>,
    },
}

#[derive(Debug)]
pub struct Element {
    pub tag: &'static str,
    pub attrs: Vec<Attr>,
    /// `..rest`: the caller's attributes, after the template's own.
    pub spread: Option<Path>,
    /// `None` for a void element (`input ...;`).
    pub children: Option<Vec<Node>>,
    /// An element the behaviour attaches to (its `on:` handlers, `ref`,
    /// and attributes that differ once enhanced): `data-mk="N"`.
    pub mark: Option<u32>,
}

#[derive(Debug)]
pub enum Attr {
    /// `name="value"`
    Static(&'static str, &'static str),
    /// `name`: present, with no value.
    Flag(&'static str),
    /// `name=(path)`
    Value(&'static str, Path),
    /// `name={ "text" (path) ... }`
    Concat(&'static str, Vec<Part>),
    /// `name[cond]`: present, with no value, while the condition holds.
    Bool(&'static str, Cond),
    /// `name=[path]`: an `Option`, present while it is set.
    Optional(&'static str, Path),
    /// `name="value" if cond` (or `(path)`, or `{ parts }`)
    When(&'static str, Vec<Part>, Cond),
    /// `name=join[ "a" if cond, { (x) "-b" } if cond, ... ]`: the tokens
    /// whose conditions hold, space-separated; absent if none do.
    Tokens(&'static str, Vec<(Vec<Part>, Option<Cond>)>),
    /// `on:event=handler` or `on:event=handler(path)`: the behaviour's.
    On(&'static str, &'static str, Option<Path>),
    /// `ref=name`: hands the element to the behaviour.
    Ref(&'static str),
}

impl Attr {
    pub fn name(&self) -> &'static str {
        match self {
            Attr::Static(n, _)
            | Attr::Flag(n)
            | Attr::Value(n, _)
            | Attr::Concat(n, _)
            | Attr::Bool(n, _)
            | Attr::Optional(n, _)
            | Attr::When(n, _, _)
            | Attr::Tokens(n, _) => n,
            Attr::On(..) => "on:",
            Attr::Ref(_) => "ref",
        }
    }

    /// Every path the attribute reads.
    pub fn paths(&self) -> Vec<Path> {
        let parts = |ps: &[Part]| -> Vec<Path> {
            ps.iter()
                .filter_map(|p| match p {
                    Part::Value(p) => Some(*p),
                    Part::Lit(_) => None,
                })
                .collect()
        };
        match self {
            Attr::Static(..) | Attr::Flag(_) | Attr::Ref(_) => vec![],
            Attr::Value(_, p) | Attr::Optional(_, p) => vec![*p],
            Attr::Concat(_, ps) => parts(ps),
            Attr::Bool(_, c) => c.paths(),
            Attr::When(_, ps, c) => {
                let mut all = parts(ps);
                all.extend(c.paths());
                all
            }
            Attr::Tokens(_, tokens) => tokens
                .iter()
                .flat_map(|(ps, c)| {
                    let mut all = parts(ps);
                    all.extend(c.iter().flat_map(Cond::paths));
                    all
                })
                .collect(),
            Attr::On(_, _, arg) => arg.iter().copied().collect(),
        }
    }

    /// An attribute that reads the behaviour's values (`b.*`): only the
    /// behaviour sets it, and keeps it up to date.
    pub fn reads_behaviour(&self) -> bool {
        !matches!(self, Attr::On(..)) && self.paths().iter().any(|p| p[0] == "b")
    }

    /// Whether the attribute is the behaviour's: absent from the server's
    /// markup, or different once enhanced.
    pub fn enhanced(&self) -> bool {
        if self.reads_behaviour() {
            return true;
        }
        match self {
            Attr::Bool(_, c) | Attr::When(_, _, c) => c.enhanced(),
            Attr::Tokens(_, tokens) => tokens
                .iter()
                .any(|(_, c)| c.as_ref().is_some_and(Cond::enhanced)),
            Attr::On(..) | Attr::Ref(_) => true,
            _ => false,
        }
    }
}

#[derive(Debug)]
pub enum Part {
    Lit(&'static str),
    Value(Path),
}

#[derive(Debug)]
pub enum Cond {
    /// `path`: true, set, or non-empty.
    Truthy(Path),
    /// `!cond`
    Not(Box<Cond>),
    /// `path == Enum::Variant` (true) or `!=` (false), by the variant's
    /// rendered value.
    Eq(Path, &'static str, bool),
    /// `path == path` (true) or `!=` (false).
    Same(Path, Path, bool),
    /// `enhanced`: false as the server renders, true once the behaviour
    /// runs.
    Enhanced,
}

impl Cond {
    pub fn paths(&self) -> Vec<Path> {
        match self {
            Cond::Truthy(p) | Cond::Eq(p, _, _) => vec![*p],
            Cond::Same(a, b, _) => vec![*a, *b],
            Cond::Not(c) => c.paths(),
            Cond::Enhanced => vec![],
        }
    }

    pub fn enhanced(&self) -> bool {
        match self {
            Cond::Enhanced => true,
            Cond::Not(c) => c.enhanced(),
            _ => false,
        }
    }
}
