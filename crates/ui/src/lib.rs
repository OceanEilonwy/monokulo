//! Monokulo's shared UI components, written once.
//!
//! Each component is declared with [`component!`]: its props as a struct and
//! its markup as a template that only reads them. From that one declaration:
//!
//! - server-rendered pages (monokulo's views, the pages site) build it with
//!   a typed builder and render it through maud: `(Button::new("Save"))`;
//! - `cargo xtask ui` writes the Solid component the POS imports
//!   ([`solid`]) and the stylebook's specimens ([`book`]), from the same
//!   description ([`ir`]) this crate's interpreter renders.
//!
//! Required props are the builder's `new` arguments, so a label or an
//! accessible name can't be left out; choices are enums, so a variant can't
//! be misspelt.
//!
//! This directory is one package for both languages: the Rust crate here,
//! and `@monokulo/ui` for npm (`package.json`). Appearance and behaviour
//! live beside the components in `web/`: CSS painted with the theme tokens,
//! and light-DOM custom elements that upgrade the components' markup.
//! Rust consumers get them from [`assets`]; npm consumers import them.

// `component!` names this crate `::ui`, as it does from outside.
extern crate self as ui;

pub mod assets;
pub mod book;
pub mod components;
pub mod ir;
mod render;
pub mod solid;

pub use components::*;
pub use render::render_component;
pub use ui_macros::{component, UiEnum, UiRecord};

#[doc(hidden)]
pub mod __private {
    pub use maud;
}

/// What a prop or record field holds, as the interpreter and the Solid
/// translation see it.
#[derive(Clone, Copy, Debug)]
pub enum Ty {
    Text,
    Bool,
    Number,
    Enum(&'static EnumInfo),
    Option(&'static Ty),
    List(&'static Ty),
    Record(fn() -> &'static RecordInfo),
    /// Markup: text, other components, or (from Rust) any maud markup.
    Slot,
    /// Attributes the caller adds to an element the template spreads them on.
    Attrs,
}

#[derive(Debug)]
pub struct EnumInfo {
    pub name: &'static str,
    pub doc: &'static str,
    pub variants: &'static [VariantInfo],
}

#[derive(Debug)]
pub struct VariantInfo {
    /// `Primary`: the Rust variant.
    pub rust: &'static str,
    /// `primary`: what it renders as.
    pub value: &'static str,
    pub doc: &'static str,
}

#[derive(Debug)]
pub struct RecordInfo {
    pub name: &'static str,
    pub doc: &'static str,
    pub fields: Vec<FieldInfo>,
}

#[derive(Debug)]
pub struct FieldInfo {
    pub name: &'static str,
    pub ty: Ty,
    pub doc: &'static str,
}

#[derive(Debug)]
pub struct PropInfo {
    pub name: &'static str,
    pub ty: Ty,
    /// An argument of `new`, and not optional in Solid.
    pub required: bool,
    /// `#[default(...)]`'s value; `None` for required, `Option` and `Attrs`
    /// props.
    pub default: Option<Value>,
    pub doc: &'static str,
}

/// A component as data: its props and its template.
#[derive(Debug)]
pub struct ComponentInfo {
    pub name: &'static str,
    pub doc: &'static str,
    pub props: Vec<PropInfo>,
    pub template: Vec<ir::Node>,
}

impl ComponentInfo {
    pub fn prop(&self, name: &str) -> Option<&PropInfo> {
        self.props.iter().find(|p| p.name == name)
    }
}

/// Implemented by [`component!`].
pub trait Component: maud::Render {
    fn info() -> &'static ComponentInfo
    where
        Self: Sized;
    /// Every prop's current value, in declaration order.
    fn props(&self) -> Vec<(&'static str, Value)>;
}

/// A prop's value, as the interpreter reads it.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Text(String),
    Bool(bool),
    Number(i64),
    Enum(&'static str),
    /// An `Option` that is `None`. `Some` is the value itself.
    Null,
    List(Vec<Value>),
    Record(Vec<(&'static str, Value)>),
    Slot(Slot),
    Attrs(Attrs),
}

impl Value {
    /// What `@if name` and `attr[name]` test: true, set, or non-empty.
    pub fn truthy(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Null => false,
            Value::Text(s) => !s.is_empty(),
            Value::List(l) => !l.is_empty(),
            Value::Slot(s) => !s.parts.is_empty(),
            Value::Attrs(a) => !a.0.is_empty(),
            Value::Number(_) | Value::Enum(_) | Value::Record(_) => true,
        }
    }
}

/// A type a prop or record field may have.
pub trait UiType {
    fn ty() -> Ty;
    fn value(&self) -> Value;
}

impl UiType for String {
    fn ty() -> Ty {
        Ty::Text
    }
    fn value(&self) -> Value {
        Value::Text(self.clone())
    }
}

impl UiType for bool {
    fn ty() -> Ty {
        Ty::Bool
    }
    fn value(&self) -> Value {
        Value::Bool(*self)
    }
}

macro_rules! numbers {
    ($($t:ty),*) => {$(
        impl UiType for $t {
            fn ty() -> Ty { Ty::Number }
            fn value(&self) -> Value {
                Value::Number(i64::try_from(*self).expect("a number a page can show"))
            }
        }
        impl AttrValue for $t {}
        impl ChildValue for $t {}
    )*};
}
numbers!(i32, i64, u32, u64, usize);

impl<T: UiType> UiType for Option<T> {
    fn ty() -> Ty {
        Ty::Option(Box::leak(Box::new(T::ty())))
    }
    fn value(&self) -> Value {
        self.as_ref().map_or(Value::Null, UiType::value)
    }
}

impl<T: UiType> UiType for Vec<T> {
    fn ty() -> Ty {
        Ty::List(Box::leak(Box::new(T::ty())))
    }
    fn value(&self) -> Value {
        Value::List(self.iter().map(UiType::value).collect())
    }
}

impl UiType for Slot {
    fn ty() -> Ty {
        Ty::Slot
    }
    fn value(&self) -> Value {
        Value::Slot(self.clone())
    }
}

impl UiType for Attrs {
    fn ty() -> Ty {
        Ty::Attrs
    }
    fn value(&self) -> Value {
        Value::Attrs(self.clone())
    }
}

/// Types a template may write into an attribute: `attr=(name)`.
pub trait AttrValue {
    fn check(&self) {}
}
impl AttrValue for String {}

/// Types a template may write as content: `(name)`.
pub trait ChildValue {
    fn check(&self) {}
}
impl ChildValue for String {}
impl ChildValue for Slot {}

/// Types `@if name` and `attr[name]` may test.
pub trait Truthy {
    fn truthy(&self) -> bool;
}
impl Truthy for bool {
    fn truthy(&self) -> bool {
        *self
    }
}
impl<T> Truthy for Option<T> {
    fn truthy(&self) -> bool {
        self.is_some()
    }
}
impl Truthy for String {
    fn truthy(&self) -> bool {
        !self.is_empty()
    }
}
impl<T> Truthy for Vec<T> {
    fn truthy(&self) -> bool {
        !self.is_empty()
    }
}

/// Content a component holds: text, other components, or markup.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Slot {
    pub parts: Vec<SlotPart>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SlotPart {
    Text(String),
    /// Markup rendered by maud. Solid can't receive it, so examples (which
    /// the Solid translation is checked against) don't use it.
    Html(String),
    Component {
        info: &'static ComponentInfo,
        props: Vec<(&'static str, Value)>,
    },
}

impl PartialEq for ComponentInfo {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl Slot {
    /// Several pieces in a row.
    pub fn many(parts: impl IntoIterator<Item = Slot>) -> Self {
        Self {
            parts: parts.into_iter().flat_map(|s| s.parts).collect(),
        }
    }
}

impl From<&str> for Slot {
    fn from(text: &str) -> Self {
        Self {
            parts: vec![SlotPart::Text(text.to_string())],
        }
    }
}

impl From<String> for Slot {
    fn from(text: String) -> Self {
        Self {
            parts: vec![SlotPart::Text(text)],
        }
    }
}

impl From<maud::Markup> for Slot {
    fn from(markup: maud::Markup) -> Self {
        Self {
            parts: vec![SlotPart::Html(markup.into_string())],
        }
    }
}

impl<C: Component> From<C> for Slot {
    fn from(component: C) -> Self {
        Self {
            parts: vec![SlotPart::Component {
                info: C::info(),
                props: component.props(),
            }],
        }
    }
}

/// Attributes added to the element a template spreads them on, in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Attrs(pub Vec<(String, String)>);

impl Attrs {
    /// Adds one. `class` and the element's own attributes belong to the
    /// component: a variant, not an override, changes them.
    pub fn push(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        assert!(
            name != "class" && name != "style",
            "`{name}` belongs to the component: add a variant instead"
        );
        self.0.push((name, value.into()));
    }
}

// A name `@if let` or `@for` binds is a reference.
impl<T: AttrValue + ?Sized> AttrValue for &T {}
impl<T: ChildValue + ?Sized> ChildValue for &T {}
impl<T: Truthy + ?Sized> Truthy for &T {
    fn truthy(&self) -> bool {
        (**self).truthy()
    }
}
