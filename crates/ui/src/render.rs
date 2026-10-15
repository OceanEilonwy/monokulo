//! The interpreter: a component's template, rendered with its props' values.

use crate::ir::{Attr, Cond, Node, Part, Path};
use crate::{Component, ComponentInfo, SlotPart, Value};

/// Renders a component into `buffer`: what each component's `maud::Render`
/// calls.
pub fn render_component<C: Component>(component: &C, buffer: &mut String) {
    render_info(C::info(), component.props(), buffer);
}

/// An interactive component renders as an island: its markup inside
/// `<mk-island>`, with its props as JSON for the behaviour that attaches in
/// the browser. Without JavaScript the markup is all there is.
pub(crate) fn render_info(
    info: &'static ComponentInfo,
    props: Vec<(&'static str, Value)>,
    buffer: &mut String,
) {
    if let Some(behaviour) = &info.behaviour {
        buffer.push_str("<mk-island component=\"");
        buffer.push_str(info.name);
        buffer.push_str("\" mount=\"");
        buffer.push_str(behaviour.mount);
        buffer.push_str("\"><script type=\"application/json\" data-props>");
        // `<` escaped, so no value can end the script early.
        let json = serde_json::to_string(&crate::json::island_props(info, &props)).expect("JSON");
        buffer.push_str(&json.replace('<', "\\u003c"));
        buffer.push_str("</script>");
    }
    let scope = Scope {
        props: &props,
        binds: Vec::new(),
    };
    nodes(&info.template, &scope, buffer);
    if info.behaviour.is_some() {
        buffer.push_str("</mk-island>");
    }
}

struct Scope<'a> {
    props: &'a [(&'static str, Value)],
    /// `@for` and `@if let` bindings, innermost last.
    binds: Vec<(&'static str, &'a Value)>,
}

impl<'a> Scope<'a> {
    fn resolve(&self, path: Path) -> &'a Value {
        let (first, rest) = path.split_first().expect("a path names something");
        let mut value = self
            .binds
            .iter()
            .rev()
            .find(|(name, _)| name == first)
            .map(|(_, v)| *v)
            .or_else(|| self.props.iter().find(|(n, _)| n == first).map(|(_, v)| v))
            .unwrap_or_else(|| panic!("the template names `{first}`, which no prop or binding is"));
        for field in rest {
            let Value::Record(fields) = value else {
                panic!("`{field}` read from something that isn't a record");
            };
            value = fields
                .iter()
                .find(|(n, _)| n == field)
                .map(|(_, v)| v)
                .unwrap_or_else(|| panic!("no field `{field}`"));
        }
        value
    }

    fn with(&self, name: &'static str, value: &'a Value) -> Scope<'a> {
        let mut binds = self.binds.clone();
        binds.push((name, value));
        Scope {
            props: self.props,
            binds,
        }
    }
}

fn nodes(list: &[Node], scope: &Scope<'_>, out: &mut String) {
    for node in list {
        match node {
            Node::Element(element) => {
                out.push('<');
                out.push_str(element.tag);
                for attr in &element.attrs {
                    attribute(attr, scope, out);
                }
                if let Some(mark) = element.mark {
                    out.push_str(&format!(" data-mk=\"{mark}\""));
                }
                if let Some(path) = element.spread {
                    let Value::Attrs(attrs) = scope.resolve(path) else {
                        panic!("a spread takes Attrs");
                    };
                    for (name, value) in &attrs.0 {
                        debug_assert!(
                            !element.attrs.iter().any(|a| a.name() == name),
                            "`{name}` belongs to the component"
                        );
                        write_attr(name, Some(value), out);
                    }
                }
                out.push('>');
                if let Some(children) = &element.children {
                    nodes(children, scope, out);
                    out.push_str("</");
                    out.push_str(element.tag);
                    out.push('>');
                }
            }
            Node::Enhanced { mark, .. } => out.push_str(&format!("<!--mk:{mark}-->")),
            Node::Text(text) => escape(text, out),
            Node::Value(path) => content(scope.resolve(path), out),
            Node::If {
                cond: c,
                then,
                otherwise,
            } => {
                if cond(c, scope) {
                    nodes(then, scope, out);
                } else {
                    nodes(otherwise, scope, out);
                }
            }
            Node::IfLet {
                bind,
                path,
                then,
                otherwise,
            } => match scope.resolve(path) {
                Value::Null => nodes(otherwise, scope, out),
                value => nodes(then, &scope.with(bind, value), out),
            },
            Node::For { bind, path, body } => {
                let Value::List(items) = scope.resolve(path) else {
                    panic!("@for runs over a list");
                };
                for item in items {
                    nodes(body, &scope.with(bind, item), out);
                }
            }
        }
    }
}

fn content(value: &Value, out: &mut String) {
    match value {
        Value::Slot(slot) => {
            for part in &slot.parts {
                match part {
                    SlotPart::Text(text) => escape(text, out),
                    SlotPart::Html(html) => out.push_str(html),
                    SlotPart::Component { info, props } => render_info(info, props.clone(), out),
                }
            }
        }
        Value::Null => {}
        other => escape(&text(other), out),
    }
}

/// A value as it reads in an attribute or as text.
fn text(value: &Value) -> String {
    match value {
        Value::Text(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Enum(s) => (*s).to_string(),
        other => panic!("{other:?} can't be written as text"),
    }
}

fn parts(list: &[Part], scope: &Scope<'_>) -> String {
    let mut s = String::new();
    for part in list {
        match part {
            Part::Lit(l) => s.push_str(l),
            Part::Value(p) => s.push_str(&text(scope.resolve(p))),
        }
    }
    s
}

fn cond(c: &Cond, scope: &Scope<'_>) -> bool {
    match c {
        Cond::Truthy(p) => scope.resolve(p).truthy(),
        Cond::Not(c) => !cond(c, scope),
        Cond::Eq(p, variant, equal) => {
            let Value::Enum(value) = scope.resolve(p) else {
                panic!("== compares an enum");
            };
            (value == variant) == *equal
        }
        Cond::Same(a, b, equal) => (scope.resolve(a) == scope.resolve(b)) == *equal,
        Cond::Enhanced => false,
    }
}

fn attribute(attr: &Attr, scope: &Scope<'_>, out: &mut String) {
    // The behaviour's to set, once it runs.
    if attr.reads_behaviour() {
        return;
    }
    match attr {
        Attr::Static(name, value) => write_attr(name, Some(value), out),
        Attr::Flag(name) => write_attr(name, None, out),
        Attr::Value(name, path) => write_attr(name, Some(&text(scope.resolve(path))), out),
        Attr::Concat(name, list) => write_attr(name, Some(&parts(list, scope)), out),
        Attr::Bool(name, c) => {
            if cond(c, scope) {
                write_attr(name, None, out);
            }
        }
        Attr::Optional(name, path) => match scope.resolve(path) {
            Value::Null => {}
            value => write_attr(name, Some(&text(value)), out),
        },
        Attr::When(name, list, c) => {
            if cond(c, scope) {
                write_attr(name, Some(&parts(list, scope)), out);
            }
        }
        Attr::Tokens(name, tokens) => {
            let present: Vec<String> = tokens
                .iter()
                .filter(|(_, c)| c.as_ref().is_none_or(|c| cond(c, scope)))
                .map(|(list, _)| parts(list, scope))
                .collect();
            if !present.is_empty() {
                write_attr(name, Some(&present.join(" ")), out);
            }
        }
        Attr::On(..) | Attr::Ref(_) => {}
    }
}

fn write_attr(name: &str, value: Option<&str>, out: &mut String) {
    out.push(' ');
    out.push_str(name);
    if let Some(value) = value {
        out.push_str("=\"");
        escape(value, out);
        out.push('"');
    }
}

/// As maud escapes: `&`, `<`, `>` and `"`.
fn escape(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
}
