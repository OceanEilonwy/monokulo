//! Props as JSON: what an island hands its behaviour in the browser, and
//! the fixtures the Solid translation is checked against. Each value is as
//! the generated TypeScript types describe it; slots are `{"$slot": [...]}`
//! of text and `{"$component": name, "props": {...}}`.

use serde_json::{json, Map, Value as Json};

use crate::{ComponentInfo, SlotPart, Ty, Value};

pub(crate) fn props_json(info: &'static ComponentInfo, props: &[(&'static str, Value)]) -> Json {
    let mut map = Map::new();
    for (name, value) in props {
        let prop = info.prop(name).expect("prop");
        match value {
            Value::Null => {}
            Value::Attrs(a) => {
                for (k, v) in &a.0 {
                    map.insert(k.clone(), Json::String(v.clone()));
                }
            }
            v => {
                map.insert((*name).to_string(), json_value(&prop.ty, v));
            }
        }
    }
    Json::Object(map)
}

pub(crate) fn json_value(ty: &Ty, value: &Value) -> Json {
    match (ty, value) {
        (Ty::Option(inner), v) => json_value(inner, v),
        (_, Value::Text(s)) => Json::String(s.clone()),
        (_, Value::Bool(b)) => Json::Bool(*b),
        (_, Value::Number(n)) => json!(n),
        (_, Value::Enum(s)) => Json::String((*s).to_string()),
        (_, Value::Null) => Json::Null,
        (Ty::List(item), Value::List(items)) => {
            Json::Array(items.iter().map(|v| json_value(item, v)).collect())
        }
        (Ty::Record(info), Value::Record(fields)) => {
            let info = info();
            let mut map = Map::new();
            for (name, v) in fields {
                // An unset field is absent, as TypeScript's optional field.
                if *v == Value::Null {
                    continue;
                }
                let field = info.fields.iter().find(|f| f.name == *name).expect("field");
                map.insert((*name).to_string(), json_value(&field.ty, v));
            }
            Json::Object(map)
        }
        (_, Value::Slot(slot)) => Json::Object(Map::from_iter([(
            "$slot".to_string(),
            Json::Array(
                slot.parts
                    .iter()
                    .map(|part| match part {
                        SlotPart::Text(t) => Json::String(t.clone()),
                        SlotPart::Component { info, props } => json!({
                            "$component": info.name,
                            "props": props_json(info, props),
                        }),
                        SlotPart::Html(_) => panic!("Solid can't take raw markup: an island's props and the examples are components and text"),
                    })
                    .collect(),
            ),
        )])),
        (ty, v) => panic!("{v:?} as {ty:?}"),
    }
}

/// An island's props: everything but slots. The behaviour renders only
/// what `@if enhanced` holds, which reads no slot, and a slot may be markup
/// only the server can render.
pub(crate) fn island_props(info: &'static ComponentInfo, props: &[(&'static str, Value)]) -> Json {
    let uses = info.behaviour.as_ref().and_then(|b| b.uses.as_ref());
    let kept: Vec<(&'static str, Value)> = props
        .iter()
        .filter(|(name, _)| uses.is_none_or(|uses| uses.contains(name)))
        .filter_map(|(name, value)| without_slots(value).map(|v| (*name, v)))
        .collect();
    props_json(info, &kept)
}

fn without_slots(value: &Value) -> Option<Value> {
    match value {
        Value::Slot(_) => None,
        Value::List(items) => Some(Value::List(
            items.iter().filter_map(without_slots).collect(),
        )),
        Value::Record(fields) => Some(Value::Record(
            fields
                .iter()
                .map(|(name, v)| (*name, without_slots(v).unwrap_or(Value::Null)))
                .collect(),
        )),
        other => Some(other.clone()),
    }
}
