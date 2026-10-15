//! The template language, rendered by the interpreter: every form a
//! template can take, on a component made for the purpose.

use maud::html;
use ui::{component, Attrs, Slot, UiEnum, UiRecord};

#[derive(Clone, Copy, Debug, PartialEq, Eq, UiEnum)]
pub enum Mood {
    Calm,
    RatherCross,
}

#[derive(Clone, Debug, UiRecord)]
pub struct Item {
    pub name: String,
    pub count: u32,
    pub mood: Mood,
    pub note: Option<String>,
}

component! {
    /// Everything at once.
    pub struct Sampler {
        title: String,
        items: Vec<Item>,
        #[default(Mood::Calm)]
        mood: Mood,
        #[default(false)]
        loud: bool,
        caption: Option<String>,
        children: Slot,
        rest: Attrs,
    }
    html {
        section class=join["sampler", { "is-" (mood) } if loud, "empty" if !items] data-mood=(mood)
            aria-label={ (title) " sampler" } hidden[!items] ..rest {
            h2 { (title) }
            @if let Some(caption) = caption { p { (caption) } } @else { p { "No caption" } }
            @if mood == Mood::RatherCross { strong { "Cross" } } @else if loud { em { "Loud" } } @else { "Quiet" }
            ul {
                @for item in items {
                    li data-count=(item.count) class="cross" if item.mood != Mood::Calm {
                        (item.name)
                        @if let Some(note) = item.note { " (" (note) ")" }
                    }
                }
            }
            input type="text" value=[caption] readonly;
            div { (children) }
        }
    }
}

fn item(name: &str, count: u32, mood: Mood, note: Option<&str>) -> Item {
    Item {
        name: name.into(),
        count,
        mood,
        note: note.map(Into::into),
    }
}

#[test]
fn every_form_renders_as_maud_would_write_it() {
    let sampler = Sampler::new(
        "Fruit",
        vec![
            item("Apples", 3, Mood::Calm, None),
            item("Pears <ripe>", 1, Mood::RatherCross, Some("soft & sweet")),
        ],
        "Picked today",
    )
    .loud(true)
    .caption("In \"season\"")
    .attr("fx-target", "#fruit");
    assert_eq!(
        sampler.render().into_string(),
        concat!(
            r##"<section class="sampler is-calm" data-mood="calm" aria-label="Fruit sampler" fx-target="#fruit">"##,
            "<h2>Fruit</h2><p>In &quot;season&quot;</p><em>Loud</em>",
            r#"<ul><li data-count="3">Apples</li><li data-count="1" class="cross">Pears &lt;ripe&gt; (soft &amp; sweet)</li></ul>"#,
            r#"<input type="text" value="In &quot;season&quot;" readonly><div>Picked today</div></section>"#,
        )
    );
}

#[test]
fn absent_options_and_empty_lists_take_the_other_branch() {
    let sampler = Sampler::new("Nothing", vec![], "").mood(Mood::RatherCross);
    assert_eq!(
        sampler.render().into_string(),
        concat!(
            r#"<section class="sampler empty" data-mood="rather-cross" aria-label="Nothing sampler" hidden>"#,
            "<h2>Nothing</h2><p>No caption</p><strong>Cross</strong><ul></ul>",
            r#"<input type="text" readonly><div></div></section>"#,
        )
    );
}

#[test]
fn slots_hold_text_components_and_markup() {
    let inner = Sampler::new("Inner", vec![], "");
    let slot = Slot::many([
        Slot::from("a < b, "),
        Slot::from(html! { b { "bold" } }),
        Slot::from(inner.clone()),
    ]);
    let html = Sampler::new("Outer", vec![], slot).render().into_string();
    assert!(
        html.contains(&format!(
            "<div>a &lt; b, <b>bold</b>{}</div>",
            inner.render().into_string()
        )),
        "{html}"
    );
}

#[test]
#[should_panic(expected = "belongs to the component")]
fn class_is_the_components_own() {
    let _ = Sampler::new("x", vec![], "").attr("class", "mine");
}

#[test]
fn the_description_lists_props_with_their_defaults() {
    use ui::{Component, Ty, Value};
    let info = Sampler::info();
    let names: Vec<_> = info.props.iter().map(|p| (p.name, p.required)).collect();
    assert_eq!(
        names,
        [
            ("title", true),
            ("items", true),
            ("mood", false),
            ("loud", false),
            ("caption", false),
            ("children", true),
            ("rest", false)
        ]
    );
    assert_eq!(
        info.prop("mood").unwrap().default,
        Some(Value::Enum("calm"))
    );
    assert!(matches!(
        info.prop("caption").unwrap().ty,
        Ty::Option(Ty::Text)
    ));
}
