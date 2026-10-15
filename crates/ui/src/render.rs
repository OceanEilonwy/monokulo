//! The interpreter: a component's template, rendered with its props' values.

use crate::ir::{Attr, Cond, Node, Part, Path};
use crate::{Component, ComponentInfo, SlotPart, Value};

/// Renders a component into `buffer`: what each component's `maud::Render`
/// calls.
pub fn render_component<C: Component>(component: &C, buffer: &mut String) {
    render_info(C::info(), component.props(), buffer);
}

/// An interactive component renders as an island: its root element names
/// it and carries the props its behaviour reads, as JSON, for the behaviour
/// that attaches in the browser. Without JavaScript the markup is all there
/// is.
pub(crate) fn render_info(
    info: &'static ComponentInfo,
    props: Vec<(&'static str, Value)>,
    buffer: &mut String,
) {
    // An interactive component's root element is its island: it names the
    // component and carries its props, for the behaviour that attaches in
    // the browser (web/islands.ts).
    let island = info.behaviour.as_ref().map(|behaviour| {
        let json = serde_json::to_string(&crate::json::island_props(info, &props)).expect("JSON");
        let mut attrs = String::new();
        write_attr("data-island", Some(info.name), &mut attrs);
        if behaviour.mount != "load" {
            write_attr("data-mount", Some(behaviour.mount), &mut attrs);
        }
        write_attr("data-props", Some(&json), &mut attrs);
        attrs
    });
    let scope = Scope {
        info,
        props: &props,
        binds: Vec::new(),
        island: std::cell::Cell::new(island),
    };
    nodes(&info.template, &scope, buffer);
}

struct Scope<'a> {
    props: &'a [(&'static str, Value)],
    info: &'static ComponentInfo,
    /// `@for` and `@if let` bindings and their types, innermost last.
    binds: Vec<(&'static str, &'a Value, Option<crate::Ty>)>,
    /// The island's attributes, for the root element (the first rendered).
    island: std::cell::Cell<Option<String>>,
}

impl<'a> Scope<'a> {
    fn resolve(&self, path: Path) -> &'a Value {
        let (first, rest) = path.split_first().expect("a path names something");
        let mut value = self
            .binds
            .iter()
            .rev()
            .find(|(name, _, _)| name == first)
            .map(|(_, v, _)| *v)
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

    /// A path's type, as the component declares it.
    fn ty(&self, path: Path) -> Option<crate::Ty> {
        let (first, rest) = path.split_first()?;
        let mut ty = match self.binds.iter().rev().find(|(name, _, _)| name == first) {
            Some((_, _, ty)) => (*ty)?,
            None => self.info.prop(first)?.ty,
        };
        for field in rest {
            let crate::Ty::Record(record) = ty else {
                return None;
            };
            ty = record().fields.iter().find(|f| f.name == *field)?.ty;
        }
        Some(ty)
    }

    fn with(&self, name: &'static str, value: &'a Value, ty: Option<crate::Ty>) -> Scope<'a> {
        let mut binds = self.binds.clone();
        binds.push((name, value, ty));
        Scope {
            info: self.info,
            props: self.props,
            binds,
            island: std::cell::Cell::new(None),
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
                if let Some(island) = scope.island.take() {
                    out.push_str(&island);
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
            Node::Enhanced { mark, fallback, .. } => {
                out.push_str(&format!("<!--mk:{mark}-->"));
                if !fallback.is_empty() {
                    nodes(fallback, scope, out);
                    out.push_str(&format!("<!--/mk:{mark}-->"));
                }
            }
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
                value => {
                    let ty = match scope.ty(path) {
                        Some(crate::Ty::Option(inner)) => Some(*inner),
                        other => other,
                    };
                    nodes(then, &scope.with(bind, value, ty), out)
                }
            },
            Node::For { bind, path, body } => {
                let Value::List(items) = scope.resolve(path) else {
                    panic!("@for runs over a list");
                };
                let ty = match scope.ty(path) {
                    Some(crate::Ty::List(inner)) => Some(*inner),
                    _ => None,
                };
                for item in items {
                    nodes(body, &scope.with(bind, item, ty), out);
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
        // An `Option` is true when set, even to an empty string.
        Cond::Truthy(p) => match scope.ty(p) {
            Some(crate::Ty::Option(_)) => *scope.resolve(p) != Value::Null,
            _ => scope.resolve(p).truthy(),
        },
        Cond::Not(c) => !cond(c, scope),
        Cond::Eq(p, variant, equal) => {
            // An unset `Option` is no variant.
            let value = match scope.resolve(p) {
                Value::Enum(value) => Some(*value),
                Value::Null => None,
                _ => panic!("== compares an enum"),
            };
            (value == Some(*variant)) == *equal
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
