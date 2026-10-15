//! The stylebook's specimens, and the fixtures the Solid translation is
//! checked against: each example, rendered here and described as props.
//! `cargo xtask ui` writes them into apps/stylebook/examples/generated and
//! crates/ui/browser/solid/generated.

use std::fmt::Write;

use serde_json::json;

use crate::json::{json_value, props_json};

use crate::components::Specimens;
use crate::render::render_info;
use crate::{ComponentInfo, PropInfo, Slot, SlotPart, Ty, Value};

/// A component's section id in the stylebook: `ProgressSteps` as
/// `component-progress-steps`, apart from any id a component renders.
pub fn anchor(name: &str) -> String {
    let mut s = String::from("component-");
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            s.push('-');
        }
        s.extend(c.to_lowercase());
    }
    s
}

/// The nav links, one per component.
pub fn nav(catalogue: &[Specimens]) -> String {
    let mut s = String::new();
    for specimens in catalogue {
        let _ = write!(
            s,
            "<a href=\"#{}\">{}</a>",
            anchor(specimens.info.name),
            specimens.info.name
        );
    }
    s.push('\n');
    s
}

/// A section per component: what it's for, its props, and each example
/// with how to write it in Rust and in Solid, and the HTML both render.
pub fn sections(catalogue: &[Specimens]) -> String {
    let mut s = String::new();
    for specimens in catalogue {
        let info = specimens.info;
        let id = anchor(info.name);
        let _ = write!(
            s,
            "<section class=\"book-section\" id=\"{id}\"><h2>{}</h2>{}",
            info.name,
            paragraphs(info.doc)
        );
        s.push_str(&props_table(info));
        s.push_str("<div class=\"book-specimens\">");
        for (i, (title, slot)) in specimens.examples.iter().enumerate() {
            let html = render(slot);
            let _ = write!(
                s,
                "<figure class=\"book-specimen\" id=\"{id}-{}\"><figcaption>{}</figcaption><div class=\"book-example\">{html}</div>\
                 <details class=\"book-source\"><summary>Rust, Solid and HTML</summary>\
                 <p class=\"book-source-label\">Rust</p><pre><code>{}</code></pre>\
                 <p class=\"book-source-label\">Solid</p><pre><code>{}</code></pre>\
                 <p class=\"book-source-label\">HTML</p><pre><code>{}</code></pre></details></figure>",
                i + 1,
                escape(title),
                escape(&rust(slot, 0)),
                escape(&solid(slot, 0)),
                escape(&html)
            );
        }
        s.push_str("</div></section>\n");
    }
    s
}

/// Each partial a hand-written stylebook page includes, by its name.
pub fn partials(catalogue: &[Specimens]) -> Vec<(&'static str, String)> {
    catalogue
        .iter()
        .flat_map(|s| {
            s.partials
                .iter()
                .map(|(name, slot)| (*name, render(slot) + "\n"))
        })
        .collect()
}

/// The examples as the Solid harness takes them: each one's props, and the
/// HTML the Rust components render for it.
pub fn fixtures(catalogue: &[Specimens]) -> String {
    let mut list = Vec::new();
    for specimens in catalogue {
        for (title, slot) in &specimens.examples {
            let [SlotPart::Component { info, props }] = slot.parts.as_slice() else {
                panic!("an example is one component");
            };
            list.push(json!({
                "component": info.name,
                "title": title,
                "props": props_json(info, props),
                "html": render(slot),
            }));
        }
    }
    let mut s = serde_json::to_string_pretty(&list).expect("JSON");
    s.push('\n');
    s
}

fn render(slot: &Slot) -> String {
    let mut s = String::new();
    for part in &slot.parts {
        match part {
            SlotPart::Component { info, props } => render_info(info, props.clone(), &mut s),
            SlotPart::Text(t) => s.push_str(&escape(t)),
            SlotPart::Html(_) => {
                panic!("examples are components and text, so Solid can render them")
            }
        }
    }
    s
}

fn paragraphs(text: &str) -> String {
    text.split("\n\n")
        .filter(|p| !p.trim().is_empty())
        .map(|p| format!("<p>{}</p>", code_spans(&escape(&p.replace('\n', " ")))))
        .collect()
}

/// Doc comments' `code` as `<code>`.
fn code_spans(text: &str) -> String {
    let mut s = String::new();
    for (i, piece) in text.split('`').enumerate() {
        if i % 2 == 1 {
            let _ = write!(s, "<code>{piece}</code>");
        } else {
            s.push_str(piece);
        }
    }
    s
}

fn props_table(info: &ComponentInfo) -> String {
    let mut s = String::from(
        "<div class=\"mk-table-scroll\"><table class=\"mk-table book-props\"><caption>Props</caption><thead><tr><th scope=\"col\">Prop</th><th scope=\"col\">Type</th><th scope=\"col\">About</th></tr></thead><tbody>",
    );
    for prop in &info.props {
        // A prop with no doc of its own reads as its enum's or record's.
        let doc = match (prop.doc, prop.ty) {
            ("", Ty::Enum(e)) => e.doc,
            ("", Ty::Option(Ty::Enum(e))) => e.doc,
            ("", Ty::List(Ty::Record(r))) => r().doc,
            (doc, _) => doc,
        };
        let about = match (&prop.default, prop.required) {
            (_, true) => format!("Required. {doc}"),
            (Some(d), _) => format!("{doc} Default: {}.", value_text(d)),
            (None, _) => doc.to_string(),
        };
        let _ = write!(
            s,
            "<tr><th scope=\"row\"><code>{}</code></th><td><code>{}</code></td><td>{}</td></tr>",
            prop.name,
            escape(&type_name(&prop.ty)),
            code_spans(&escape(about.trim()))
        );
    }
    s.push_str("</tbody></table></div>");
    s
}

fn type_name(ty: &Ty) -> String {
    match ty {
        Ty::Text => "String".into(),
        Ty::Bool => "bool".into(),
        Ty::Number => "number".into(),
        Ty::Slot => "Slot".into(),
        Ty::Attrs => "Attrs".into(),
        Ty::Option(inner) => format!("Option<{}>", type_name(inner)),
        Ty::List(inner) => format!("Vec<{}>", type_name(inner)),
        Ty::Enum(info) => {
            let values: Vec<&str> = info.variants.iter().map(|v| v.value).collect();
            format!("{} ({})", info.name, values.join(" | "))
        }
        Ty::Record(info) => info().name.into(),
    }
}

fn value_text(value: &Value) -> String {
    match value {
        Value::Text(s) => format!("{s:?}"),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Enum(s) => (*s).to_string(),
        _ => String::new(),
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The props that differ from their defaults, in declaration order.
fn set_props<'a>(
    info: &'static ComponentInfo,
    props: &'a [(&'static str, Value)],
) -> Vec<(&'static PropInfo, &'a Value)> {
    props
        .iter()
        .map(|(name, value)| (info.prop(name).expect("prop"), value))
        .filter(|(prop, value)| match value {
            Value::Null => false,
            Value::Attrs(a) => !a.0.is_empty(),
            v => prop.default.as_ref() != Some(v),
        })
        .collect()
}

// --- Rust -----------------------------------------------------------------

fn rust(slot: &Slot, depth: usize) -> String {
    let parts: Vec<String> = slot
        .parts
        .iter()
        .map(|part| match part {
            SlotPart::Text(t) => format!("{t:?}"),
            SlotPart::Html(_) => "html! { ... }".into(),
            SlotPart::Component { info, props } => rust_component(info, props, depth),
        })
        .collect();
    match parts.as_slice() {
        [one] => one.clone(),
        _ => format!("Slot::many([{}])", parts.join(", ")),
    }
}

fn rust_component(
    info: &'static ComponentInfo,
    props: &[(&'static str, Value)],
    depth: usize,
) -> String {
    let set = set_props(info, props);
    let args: Vec<String> = set
        .iter()
        .filter(|(p, _)| p.required)
        .map(|(p, v)| rust_value(&p.ty, v, depth))
        .collect();
    let mut s = format!("{}::new({})", info.name, args.join(", "));
    for (prop, value) in set.iter().filter(|(p, _)| !p.required) {
        let indent = "    ".repeat(depth + 1);
        match value {
            Value::Attrs(attrs) => {
                for (name, v) in &attrs.0 {
                    let _ = write!(s, "\n{indent}.attr({name:?}, {v:?})");
                }
            }
            v => {
                let _ = write!(
                    s,
                    "\n{indent}.{}({})",
                    prop.name,
                    rust_value(&prop.ty, v, depth + 1)
                );
            }
        }
    }
    s
}

fn rust_value(ty: &Ty, value: &Value, depth: usize) -> String {
    match (ty, value) {
        (Ty::Option(inner), v) => rust_value(inner, v, depth),
        (_, Value::Text(s)) => format!("{s:?}"),
        (_, Value::Bool(b)) => b.to_string(),
        (_, Value::Number(n)) => n.to_string(),
        (Ty::Enum(info), Value::Enum(v)) => {
            let variant = info
                .variants
                .iter()
                .find(|x| x.value == *v)
                .expect("variant");
            format!("{}::{}", info.name, variant.rust)
        }
        (_, Value::Slot(slot)) => rust(slot, depth),
        (Ty::List(item), Value::List(items)) => {
            let indent = "    ".repeat(depth + 1);
            let items: Vec<String> = items
                .iter()
                .map(|v| format!("\n{indent}{},", rust_value(item, v, depth + 1)))
                .collect();
            format!("vec![{}\n{}]", items.join(""), "    ".repeat(depth))
        }
        (Ty::Record(info), Value::Record(fields)) if info().builder.is_some() => {
            let info = info();
            let builder = info.builder.as_ref().expect("a builder");
            let field = |name: &str| info.fields.iter().find(|f| f.name == name).expect("field");
            let value = |name: &str| &fields.iter().find(|(n, _)| *n == name).expect("value").1;
            let args: Vec<String> = builder
                .new
                .iter()
                .map(|name| rust_value(&field(name).ty, value(name), depth))
                .collect();
            let mut s = format!("{}::new({})", info.name, args.join(", "));
            for (name, v) in fields {
                if builder.new.contains(name) || builder.skip.contains(name) {
                    continue;
                }
                match v {
                    Value::Null | Value::Bool(false) => {}
                    v => {
                        let _ = write!(s, ".{name}({})", rust_value(&field(name).ty, v, depth));
                    }
                }
            }
            s
        }
        (Ty::Record(info), Value::Record(fields)) => {
            let info = info();
            let fields: Vec<String> = fields
                .iter()
                .map(|(name, v)| {
                    let field = info.fields.iter().find(|f| f.name == *name).expect("field");
                    let v = rust_value(&field.ty, v, depth);
                    let v = if matches!(field.ty, Ty::Text) {
                        format!("{v}.into()")
                    } else {
                        v
                    };
                    format!("{name}: {v}")
                })
                .collect();
            format!("{} {{ {} }}", info.name, fields.join(", "))
        }
        (ty, v) => panic!("{v:?} as {ty:?}"),
    }
}

// --- Solid ----------------------------------------------------------------

fn solid(slot: &Slot, depth: usize) -> String {
    slot.parts
        .iter()
        .map(|part| match part {
            SlotPart::Text(t) => t.clone(),
            SlotPart::Html(_) => "…".into(),
            SlotPart::Component { info, props } => solid_component(info, props, depth),
        })
        .collect::<Vec<_>>()
        .join("")
}

fn solid_component(
    info: &'static ComponentInfo,
    props: &[(&'static str, Value)],
    depth: usize,
) -> String {
    let set = set_props(info, props);
    let mut attrs = Vec::new();
    let mut children = None;
    for (prop, value) in &set {
        match value {
            Value::Slot(slot) if prop.name == "children" => children = Some(solid(slot, depth + 1)),
            Value::Attrs(a) => {
                for (name, v) in &a.0 {
                    attrs.push(format!("{name}={v:?}"));
                }
            }
            Value::Text(s) => attrs.push(format!("{}={s:?}", prop.name)),
            Value::Enum(s) => attrs.push(format!("{}={s:?}", prop.name)),
            Value::Bool(true) => attrs.push(prop.name.to_string()),
            v => attrs.push(format!(
                "{}={{{}}}",
                prop.name,
                serde_json::to_string_pretty(&json_value(&prop.ty, v)).expect("JSON")
            )),
        }
    }
    let open = if attrs.is_empty() {
        info.name.to_string()
    } else {
        format!("{} {}", info.name, attrs.join(" "))
    };
    match children {
        Some(c) => format!("<{open}>{c}</{}>", info.name),
        None => format!("<{open} />"),
    }
}
