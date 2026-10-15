//! The macros behind the `ui` crate.
//!
//! `component!` takes a component's props, as a struct, and its template, in
//! a small maud-like language that only reads props: no Rust runs inside a
//! template, so every template can be rendered by `ui`'s interpreter and
//! translated to a Solid component alike. The macro emits:
//!
//! - the struct, with private fields, a `new` taking the required props in
//!   order and a setter for every optional one;
//! - its `ui::Component` impl: the props' types and defaults and the
//!   template, described as data (`ui::ir`);
//! - a function that is never called but type-checks every prop, field and
//!   enum variant the template names, so a typo fails to compile.

use proc_macro::TokenStream;
use proc_macro2::{Delimiter, Span, TokenStream as Tokens, TokenTree};
use quote::{format_ident, quote, ToTokens};
use syn::parse::{Parse, ParseStream};
use syn::{Attribute, Expr, Fields, Ident, ItemStruct, LitStr, Type, Visibility};

// ---------------------------------------------------------------------------
// Names shared by the derives and templates
// ---------------------------------------------------------------------------

/// `CamelCase` to `camel-case`: the string an enum variant renders as, in
/// attributes, in the Solid types and in templates' comparisons.
fn kebab(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('-');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn doc_of(attrs: &[Attribute]) -> String {
    let mut lines = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        if let syn::Meta::NameValue(nv) = &attr.meta {
            if let Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) = &nv.value
            {
                lines.push(s.value().trim().to_string());
            }
        }
    }
    lines.join("\n").trim().to_string()
}

// ---------------------------------------------------------------------------
// #[derive(UiEnum)]
// ---------------------------------------------------------------------------

/// A closed set of choices a prop takes: unit variants only. Each renders
/// as its kebab-case name and becomes a string union in TypeScript.
#[proc_macro_derive(UiEnum)]
pub fn derive_ui_enum(input: TokenStream) -> TokenStream {
    let item = syn::parse_macro_input!(input as syn::ItemEnum);
    let name = &item.ident;
    let doc = doc_of(&item.attrs);
    let mut idents = Vec::new();
    let mut strings = Vec::new();
    let mut docs = Vec::new();
    for variant in &item.variants {
        if !matches!(variant.fields, Fields::Unit) {
            return syn::Error::new_spanned(variant, "UiEnum variants carry no data")
                .to_compile_error()
                .into();
        }
        idents.push(variant.ident.clone());
        strings.push(kebab(&variant.ident.to_string()));
        docs.push(doc_of(&variant.attrs));
    }
    let rust_names: Vec<String> = idents.iter().map(|i| i.to_string()).collect();
    let name_str = name.to_string();
    quote! {
        impl #name {
            /// The variant as it renders: kebab-case.
            pub const fn as_str(self) -> &'static str {
                match self { #( Self::#idents => #strings, )* }
            }
        }
        impl ::core::fmt::Display for #name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.write_str(self.as_str())
            }
        }
        impl ::ui::UiType for #name {
            fn ty() -> ::ui::Ty {
                static INFO: ::ui::EnumInfo = ::ui::EnumInfo {
                    name: #name_str,
                    doc: #doc,
                    variants: &[ #( ::ui::VariantInfo { rust: #rust_names, value: #strings, doc: #docs } ),* ],
                };
                ::ui::Ty::Enum(&INFO)
            }
            fn value(&self) -> ::ui::Value { ::ui::Value::Enum(self.as_str()) }
        }
        impl ::ui::AttrValue for #name {}
        impl ::ui::ChildValue for #name {}
    }
    .into()
}

// ---------------------------------------------------------------------------
// #[derive(UiRecord)]
// ---------------------------------------------------------------------------

/// An item of a list prop: a plain struct of named fields, each a type the
/// templates can read.
#[proc_macro_derive(UiRecord)]
pub fn derive_ui_record(input: TokenStream) -> TokenStream {
    let item = syn::parse_macro_input!(input as ItemStruct);
    let name = &item.ident;
    let name_str = name.to_string();
    let doc = doc_of(&item.attrs);
    let Fields::Named(fields) = &item.fields else {
        return syn::Error::new_spanned(&item, "UiRecord needs named fields")
            .to_compile_error()
            .into();
    };
    let mut infos = Vec::new();
    let mut values = Vec::new();
    for field in &fields.named {
        let ident = field.ident.as_ref().expect("named");
        let field_name = ident.to_string();
        let ty = &field.ty;
        let field_doc = doc_of(&field.attrs);
        infos.push(quote! {
            ::ui::FieldInfo { name: #field_name, ty: <#ty as ::ui::UiType>::ty(), doc: #field_doc }
        });
        values.push(quote! { (#field_name, ::ui::UiType::value(&self.#ident)) });
    }
    quote! {
        impl ::ui::UiType for #name {
            fn ty() -> ::ui::Ty {
                fn info() -> &'static ::ui::RecordInfo {
                    static INFO: ::std::sync::OnceLock<::ui::RecordInfo> = ::std::sync::OnceLock::new();
                    INFO.get_or_init(|| ::ui::RecordInfo {
                        name: #name_str,
                        doc: #doc,
                        fields: vec![ #(#infos),* ],
                    })
                }
                ::ui::Ty::Record(info)
            }
            fn value(&self) -> ::ui::Value { ::ui::Value::Record(vec![ #(#values),* ]) }
        }
    }
    .into()
}

// ---------------------------------------------------------------------------
// component!
// ---------------------------------------------------------------------------

struct ComponentInput {
    item: ItemStruct,
    template: Tokens,
}

impl Parse for ComponentInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let item: ItemStruct = input.parse()?;
        let keyword: Ident = input.parse()?;
        if keyword != "html" {
            return Err(syn::Error::new(keyword.span(), "expected `html { ... }`"));
        }
        let content;
        syn::braced!(content in input);
        let template: Tokens = content.parse()?;
        if !input.is_empty() {
            return Err(input.error("nothing may follow the template"));
        }
        Ok(Self { item, template })
    }
}

enum PropKind {
    Required,
    /// `Option<T>`: unset is `None`; the setter takes a `T`.
    Optional(Type),
    Default(Expr),
    /// `Attrs`: what the caller adds to the element the template spreads it on.
    Rest,
}

struct Prop {
    ident: Ident,
    ty: Type,
    kind: PropKind,
    doc: String,
}

fn option_inner(ty: &Type) -> Option<Type> {
    let Type::Path(path) = ty else { return None };
    let last = path.path.segments.last()?;
    if last.ident != "Option" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &last.arguments else {
        return None;
    };
    match args.args.first()? {
        syn::GenericArgument::Type(inner) => Some(inner.clone()),
        _ => None,
    }
}

fn is_attrs(ty: &Type) -> bool {
    matches!(ty, Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "Attrs"))
}

/// `component! { /// docs  pub struct Name { props } html { template } }`
#[proc_macro]
pub fn component(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as ComponentInput);
    match expand_component(input) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand_component(input: ComponentInput) -> syn::Result<Tokens> {
    let ComponentInput { mut item, template } = input;
    let name = item.ident.clone();
    let name_str = name.to_string();
    let doc = doc_of(&item.attrs);
    let Fields::Named(fields) = &mut item.fields else {
        return Err(syn::Error::new_spanned(
            &item,
            "a component's props are named fields",
        ));
    };

    let mut props = Vec::new();
    for field in fields.named.iter_mut() {
        let ident = field.ident.clone().expect("named");
        let mut default = None;
        let mut kept = Vec::new();
        for attr in field.attrs.drain(..) {
            if attr.path().is_ident("default") {
                default = Some(attr.parse_args::<Expr>()?);
            } else {
                kept.push(attr);
            }
        }
        field.attrs = kept;
        field.vis = Visibility::Inherited;
        let ty = field.ty.clone();
        let kind = if is_attrs(&ty) {
            PropKind::Rest
        } else if let Some(expr) = default {
            PropKind::Default(expr)
        } else if let Some(inner) = option_inner(&ty) {
            PropKind::Optional(inner)
        } else {
            PropKind::Required
        };
        props.push(Prop {
            doc: doc_of(&field.attrs),
            ident,
            ty,
            kind,
        });
    }

    let nodes = Parser::new(template).nodes()?;

    // The constructor and setters.
    let required: Vec<&Prop> = props
        .iter()
        .filter(|p| matches!(p.kind, PropKind::Required))
        .collect();
    let new_args = required.iter().map(|p| {
        let (ident, ty) = (&p.ident, &p.ty);
        quote! { #ident: impl ::core::convert::Into<#ty> }
    });
    let new_fields = props.iter().map(|p| {
        let ident = &p.ident;
        match &p.kind {
            PropKind::Required => quote! { #ident: #ident.into() },
            PropKind::Optional(_) => quote! { #ident: ::core::option::Option::None },
            PropKind::Default(expr) => quote! { #ident: #expr },
            PropKind::Rest => quote! { #ident: ::ui::Attrs::default() },
        }
    });
    let mut setters = Vec::new();
    for p in &props {
        let ident = &p.ident;
        let doc = if p.doc.is_empty() {
            format!("Sets `{ident}`.")
        } else {
            p.doc.clone()
        };
        match &p.kind {
            PropKind::Required => {}
            PropKind::Optional(inner) => {
                let maybe = format_ident!("maybe_{}", ident);
                setters.push(quote! {
                    #[doc = #doc]
                    #[must_use]
                    pub fn #ident(mut self, value: impl ::core::convert::Into<#inner>) -> Self {
                        self.#ident = ::core::option::Option::Some(value.into());
                        self
                    }
                    #[doc = #doc]
                    #[must_use]
                    pub fn #maybe(mut self, value: ::core::option::Option<#inner>) -> Self {
                        self.#ident = value;
                        self
                    }
                });
            }
            PropKind::Default(_) => {
                let ty = &p.ty;
                setters.push(quote! {
                    #[doc = #doc]
                    #[must_use]
                    pub fn #ident(mut self, value: impl ::core::convert::Into<#ty>) -> Self {
                        self.#ident = value.into();
                        self
                    }
                });
            }
            PropKind::Rest => setters.push(quote! {
                /// Adds an attribute to the element the template spreads the
                /// caller's attributes on (`fx-*`, `form`, `data-*`, ...).
                #[must_use]
                pub fn attr(mut self, name: impl ::core::convert::Into<::std::string::String>, value: impl ::core::convert::Into<::std::string::String>) -> Self {
                    self.#ident.push(name, value);
                    self
                }
                /// Adds an attribute with no value.
                #[must_use]
                pub fn flag(mut self, name: impl ::core::convert::Into<::std::string::String>) -> Self {
                    self.#ident.push(name, "");
                    self
                }
            }),
        }
    }

    // The description.
    let prop_infos = props.iter().map(|p| {
        let name = p.ident.to_string();
        let ty = &p.ty;
        let doc = &p.doc;
        let (required, default) = match &p.kind {
            PropKind::Required => (true, quote! { ::core::option::Option::None }),
            PropKind::Default(expr) => (
                false,
                quote! { ::core::option::Option::Some(::ui::UiType::value(&{ let value: #ty = #expr; value })) },
            ),
            _ => (false, quote! { ::core::option::Option::None }),
        };
        quote! {
            ::ui::PropInfo { name: #name, ty: <#ty as ::ui::UiType>::ty(), required: #required, default: #default, doc: #doc }
        }
    });
    let values = props.iter().map(|p| {
        let ident = &p.ident;
        let name = ident.to_string();
        quote! { (#name, ::ui::UiType::value(&self.#ident)) }
    });
    let ir = nodes.iter().map(Node::ir);

    // Every name the template reads, type-checked.
    let mut scope = Vec::new();
    let checks: Vec<Tokens> = nodes.iter().map(|n| n.check(&mut scope)).collect();
    let check_fn = format_ident!("__ui_check_{}", name);

    Ok(quote! {
        #[derive(Clone, Debug)]
        #item

        impl #name {
            /// The component with its required props; the rest take their
            /// defaults until set.
            #[must_use]
            pub fn new(#(#new_args),*) -> Self {
                Self { #(#new_fields),* }
            }
            #(#setters)*
        }

        impl ::ui::Component for #name {
            fn info() -> &'static ::ui::ComponentInfo {
                static INFO: ::std::sync::OnceLock<::ui::ComponentInfo> = ::std::sync::OnceLock::new();
                INFO.get_or_init(|| ::ui::ComponentInfo {
                    name: #name_str,
                    doc: #doc,
                    props: vec![ #(#prop_infos),* ],
                    template: vec![ #(#ir),* ],
                })
            }
            fn props(&self) -> ::std::vec::Vec<(&'static str, ::ui::Value)> {
                vec![ #(#values),* ]
            }
        }

        impl ::ui::__private::maud::Render for #name {
            fn render_to(&self, buffer: &mut ::std::string::String) {
                ::ui::render_component(self, buffer);
            }
        }

        #[allow(dead_code, unused_variables, non_snake_case, clippy::all)]
        fn #check_fn(props: &#name) {
            #(#checks)*
        }
    })
}

// ---------------------------------------------------------------------------
// The template language
// ---------------------------------------------------------------------------

/// `name` or `name.field`: a prop, or a name a loop or `if let` binds.
#[derive(Clone)]
struct Path(Vec<Ident>);

impl Path {
    fn strings(&self) -> Vec<String> {
        self.0.iter().map(Ident::to_string).collect()
    }
    fn ir(&self) -> Tokens {
        let parts = self.strings();
        quote! { &[ #(#parts),* ] }
    }
    /// The Rust expression it names, inside the check function: a bound
    /// name as itself, anything else as a prop.
    fn expr(&self, scope: &[Ident]) -> Tokens {
        let first = &self.0[0];
        let rest = &self.0[1..];
        if scope.iter().any(|s| s == first) {
            quote! { #first #(.#rest)* }
        } else {
            quote! { props.#first #(.#rest)* }
        }
    }
}

enum Cond {
    Truthy(Path),
    Not(Box<Cond>),
    Eq(Path, syn::Path, bool),
}

impl Cond {
    fn ir(&self) -> Tokens {
        match self {
            Cond::Truthy(p) => {
                let p = p.ir();
                quote! { ::ui::ir::Cond::Truthy(#p) }
            }
            Cond::Not(c) => {
                let c = c.ir();
                quote! { ::ui::ir::Cond::Not(::std::boxed::Box::new(#c)) }
            }
            Cond::Eq(p, variant, equal) => {
                let p = p.ir();
                let value = kebab(&variant.segments.last().expect("variant").ident.to_string());
                quote! { ::ui::ir::Cond::Eq(#p, #value, #equal) }
            }
        }
    }
    fn check(&self, scope: &[Ident]) -> Tokens {
        match self {
            Cond::Truthy(p) => {
                let e = p.expr(scope);
                quote! { let _ = ::ui::Truthy::truthy(&#e); }
            }
            Cond::Not(c) => c.check(scope),
            Cond::Eq(p, variant, _) => {
                let e = p.expr(scope);
                quote! { let _ = matches!(#e, #variant); }
            }
        }
    }
}

enum Part {
    Lit(String),
    Value(Path),
}

impl Part {
    fn ir(&self) -> Tokens {
        match self {
            Part::Lit(s) => quote! { ::ui::ir::Part::Lit(#s) },
            Part::Value(p) => {
                let p = p.ir();
                quote! { ::ui::ir::Part::Value(#p) }
            }
        }
    }
    fn check(&self, scope: &[Ident]) -> Tokens {
        match self {
            Part::Lit(_) => quote! {},
            Part::Value(p) => {
                let e = p.expr(scope);
                quote! { ::ui::AttrValue::check(&#e); }
            }
        }
    }
}

enum Attr {
    Static(String, String),
    Flag(String),
    Value(String, Path),
    Concat(String, Vec<Part>),
    Bool(String, Cond),
    Optional(String, Path),
    When(String, String, Cond),
    Tokens(String, Vec<(Vec<Part>, Option<Cond>)>),
}

impl Attr {
    fn ir(&self) -> Tokens {
        match self {
            Attr::Static(n, v) => quote! { ::ui::ir::Attr::Static(#n, #v) },
            Attr::Flag(n) => quote! { ::ui::ir::Attr::Flag(#n) },
            Attr::Value(n, p) => {
                let p = p.ir();
                quote! { ::ui::ir::Attr::Value(#n, #p) }
            }
            Attr::Concat(n, parts) => {
                let parts = parts.iter().map(Part::ir);
                quote! { ::ui::ir::Attr::Concat(#n, vec![ #(#parts),* ]) }
            }
            Attr::Bool(n, c) => {
                let c = c.ir();
                quote! { ::ui::ir::Attr::Bool(#n, #c) }
            }
            Attr::Optional(n, p) => {
                let p = p.ir();
                quote! { ::ui::ir::Attr::Optional(#n, #p) }
            }
            Attr::When(n, v, c) => {
                let c = c.ir();
                quote! { ::ui::ir::Attr::When(#n, #v, #c) }
            }
            Attr::Tokens(n, tokens) => {
                let tokens = tokens.iter().map(|(parts, cond)| {
                    let parts = parts.iter().map(Part::ir);
                    let cond = match cond {
                        Some(c) => {
                            let c = c.ir();
                            quote! { ::core::option::Option::Some(#c) }
                        }
                        None => quote! { ::core::option::Option::None },
                    };
                    quote! { (vec![ #(#parts),* ], #cond) }
                });
                quote! { ::ui::ir::Attr::Tokens(#n, vec![ #(#tokens),* ]) }
            }
        }
    }
    fn check(&self, scope: &[Ident]) -> Tokens {
        match self {
            Attr::Static(..) | Attr::Flag(_) => quote! {},
            Attr::Value(_, p) => {
                let e = p.expr(scope);
                quote! { ::ui::AttrValue::check(&#e); }
            }
            Attr::Concat(_, parts) => {
                let checks = parts.iter().map(|p| p.check(scope));
                quote! { #(#checks)* }
            }
            Attr::Bool(_, c) | Attr::When(_, _, c) => c.check(scope),
            Attr::Optional(_, p) => {
                let e = p.expr(scope);
                quote! { if let ::core::option::Option::Some(value) = &#e { ::ui::AttrValue::check(value); } }
            }
            Attr::Tokens(_, tokens) => {
                let checks = tokens.iter().map(|(parts, cond)| {
                    let parts = parts.iter().map(|p| p.check(scope));
                    let cond = cond.as_ref().map(|c| c.check(scope));
                    quote! { #(#parts)* #cond }
                });
                quote! { #(#checks)* }
            }
        }
    }
}

enum Node {
    Element {
        tag: String,
        attrs: Vec<Attr>,
        spread: Option<Path>,
        children: Option<Vec<Node>>,
    },
    Text(String),
    Value(Path),
    If {
        cond: Cond,
        then: Vec<Node>,
        otherwise: Vec<Node>,
    },
    IfLet {
        bind: Ident,
        path: Path,
        then: Vec<Node>,
        otherwise: Vec<Node>,
    },
    For {
        bind: Ident,
        path: Path,
        body: Vec<Node>,
    },
}

impl Node {
    fn ir(&self) -> Tokens {
        match self {
            Node::Element {
                tag,
                attrs,
                spread,
                children,
            } => {
                let attrs = attrs.iter().map(Attr::ir);
                let spread = match spread {
                    Some(p) => {
                        let p = p.ir();
                        quote! { ::core::option::Option::Some(#p) }
                    }
                    None => quote! { ::core::option::Option::None },
                };
                let children = match children {
                    Some(c) => {
                        let c = c.iter().map(Node::ir);
                        quote! { ::core::option::Option::Some(vec![ #(#c),* ]) }
                    }
                    None => quote! { ::core::option::Option::None },
                };
                quote! {
                    ::ui::ir::Node::Element(::ui::ir::Element { tag: #tag, attrs: vec![ #(#attrs),* ], spread: #spread, children: #children })
                }
            }
            Node::Text(s) => quote! { ::ui::ir::Node::Text(#s) },
            Node::Value(p) => {
                let p = p.ir();
                quote! { ::ui::ir::Node::Value(#p) }
            }
            Node::If {
                cond,
                then,
                otherwise,
            } => {
                let cond = cond.ir();
                let then = then.iter().map(Node::ir);
                let otherwise = otherwise.iter().map(Node::ir);
                quote! { ::ui::ir::Node::If { cond: #cond, then: vec![ #(#then),* ], otherwise: vec![ #(#otherwise),* ] } }
            }
            Node::IfLet {
                bind,
                path,
                then,
                otherwise,
            } => {
                let bind = bind.to_string();
                let path = path.ir();
                let then = then.iter().map(Node::ir);
                let otherwise = otherwise.iter().map(Node::ir);
                quote! { ::ui::ir::Node::IfLet { bind: #bind, path: #path, then: vec![ #(#then),* ], otherwise: vec![ #(#otherwise),* ] } }
            }
            Node::For { bind, path, body } => {
                let bind = bind.to_string();
                let path = path.ir();
                let body = body.iter().map(Node::ir);
                quote! { ::ui::ir::Node::For { bind: #bind, path: #path, body: vec![ #(#body),* ] } }
            }
        }
    }

    fn check(&self, scope: &mut Vec<Ident>) -> Tokens {
        match self {
            Node::Element {
                attrs,
                spread,
                children,
                ..
            } => {
                let attrs: Vec<Tokens> = attrs.iter().map(|a| a.check(scope)).collect();
                let spread = spread.as_ref().map(|p| {
                    let e = p.expr(scope);
                    quote! { let _: &::ui::Attrs = &#e; }
                });
                let children: Vec<Tokens> =
                    children.iter().flatten().map(|c| c.check(scope)).collect();
                quote! { #(#attrs)* #spread #(#children)* }
            }
            Node::Text(_) => quote! {},
            Node::Value(p) => {
                let e = p.expr(scope);
                quote! { ::ui::ChildValue::check(&#e); }
            }
            Node::If {
                cond,
                then,
                otherwise,
            } => {
                let cond = cond.check(scope);
                let then: Vec<Tokens> = then.iter().map(|n| n.check(scope)).collect();
                let otherwise: Vec<Tokens> = otherwise.iter().map(|n| n.check(scope)).collect();
                quote! { #cond { #(#then)* } { #(#otherwise)* } }
            }
            Node::IfLet {
                bind,
                path,
                then,
                otherwise,
            } => {
                let e = path.expr(scope);
                let otherwise: Vec<Tokens> = otherwise.iter().map(|n| n.check(scope)).collect();
                scope.push(bind.clone());
                let then: Vec<Tokens> = then.iter().map(|n| n.check(scope)).collect();
                scope.pop();
                quote! {
                    if let ::core::option::Option::Some(#bind) = &#e { #(#then)* }
                    { #(#otherwise)* }
                }
            }
            Node::For { bind, path, body } => {
                let e = path.expr(scope);
                scope.push(bind.clone());
                let body: Vec<Tokens> = body.iter().map(|n| n.check(scope)).collect();
                scope.pop();
                quote! { for #bind in #e.iter() { #(#body)* } }
            }
        }
    }
}

struct Parser {
    tokens: Vec<TokenTree>,
    at: usize,
}

type Result<T> = syn::Result<T>;

impl Parser {
    fn new(stream: Tokens) -> Self {
        Self {
            tokens: stream.into_iter().collect(),
            at: 0,
        }
    }

    fn peek(&self) -> Option<&TokenTree> {
        self.tokens.get(self.at)
    }

    fn peek_at(&self, offset: usize) -> Option<&TokenTree> {
        self.tokens.get(self.at + offset)
    }

    fn next(&mut self) -> Option<TokenTree> {
        let t = self.tokens.get(self.at).cloned();
        self.at += 1;
        t
    }

    fn span(&self) -> Span {
        self.peek()
            .map(TokenTree::span)
            .or_else(|| self.tokens.last().map(TokenTree::span))
            .unwrap_or_else(Span::call_site)
    }

    fn error<T>(&self, message: &str) -> Result<T> {
        Err(syn::Error::new(self.span(), message))
    }

    fn is_punct(&self, c: char) -> bool {
        matches!(self.peek(), Some(TokenTree::Punct(p)) if p.as_char() == c)
    }

    fn is_punct_at(&self, offset: usize, c: char) -> bool {
        matches!(self.peek_at(offset), Some(TokenTree::Punct(p)) if p.as_char() == c)
    }

    fn is_ident(&self, word: &str) -> bool {
        matches!(self.peek(), Some(TokenTree::Ident(i)) if i == word)
    }

    fn is_group(&self, delimiter: Delimiter) -> bool {
        matches!(self.peek(), Some(TokenTree::Group(g)) if g.delimiter() == delimiter)
    }

    fn group(&mut self, delimiter: Delimiter) -> Result<Parser> {
        match self.next() {
            Some(TokenTree::Group(g)) if g.delimiter() == delimiter => Ok(Parser::new(g.stream())),
            _ => {
                self.at -= 1;
                self.error("expected a group")
            }
        }
    }

    fn ident(&mut self) -> Result<Ident> {
        match self.next() {
            Some(TokenTree::Ident(i)) => Ok(i),
            _ => {
                self.at -= 1;
                self.error("expected a name")
            }
        }
    }

    fn expect_punct(&mut self, c: char) -> Result<()> {
        if self.is_punct(c) {
            self.at += 1;
            Ok(())
        } else {
            self.error(&format!("expected `{c}`"))
        }
    }

    fn lit_str(&mut self) -> Result<Option<String>> {
        if let Some(TokenTree::Literal(lit)) = self.peek() {
            let parsed: LitStr = syn::parse2(lit.to_token_stream())
                .map_err(|_| syn::Error::new(lit.span(), "only string literals"))?;
            self.at += 1;
            return Ok(Some(parsed.value()));
        }
        Ok(None)
    }

    /// `data-variant`, `aria-label`, `mk-select`: names with hyphens.
    fn hyphenated(&mut self) -> Result<String> {
        let mut name = self.ident()?.to_string();
        while self.is_punct('-') && matches!(self.peek_at(1), Some(TokenTree::Ident(_))) {
            self.at += 1;
            name.push('-');
            name.push_str(&self.ident()?.to_string());
        }
        Ok(name)
    }

    fn path(&mut self) -> Result<Path> {
        let mut parts = vec![self.ident()?];
        while self.is_punct('.')
            && !self.is_punct_at(1, '.')
            && matches!(self.peek_at(1), Some(TokenTree::Ident(_)))
        {
            self.at += 1;
            parts.push(self.ident()?);
        }
        Ok(Path(parts))
    }

    fn whole_path(mut self) -> Result<Path> {
        let path = self.path()?;
        if self.peek().is_some() {
            return self.error("expected only a name");
        }
        Ok(path)
    }

    /// `name`, `!name`, `name == Enum::Variant`, `name != Enum::Variant`.
    fn cond(&mut self) -> Result<Cond> {
        if self.is_punct('!') {
            self.at += 1;
            return Ok(Cond::Not(Box::new(self.cond()?)));
        }
        let path = self.path()?;
        let equal = if self.is_punct('=') && self.is_punct_at(1, '=') {
            true
        } else if self.is_punct('!') && self.is_punct_at(1, '=') {
            false
        } else {
            return Ok(Cond::Truthy(path));
        };
        self.at += 2;
        let mut variant = Tokens::new();
        variant.extend([self
            .next()
            .ok_or_else(|| syn::Error::new(self.span(), "expected a variant"))?]);
        while self.is_punct(':') && self.is_punct_at(1, ':') {
            for _ in 0..2 {
                variant.extend([self.next().expect("::")]);
            }
            variant.extend([self.ident()?.into_token_stream()]);
        }
        let variant: syn::Path = syn::parse2(variant)?;
        if variant.segments.len() < 2 {
            return Err(syn::Error::new_spanned(
                variant,
                "name the enum: `Enum::Variant`",
            ));
        }
        Ok(Cond::Eq(path, variant, equal))
    }

    /// The parts inside `{ "lit" (path) ... }`.
    fn parts(mut self) -> Result<Vec<Part>> {
        let mut parts = Vec::new();
        while self.peek().is_some() {
            if let Some(s) = self.lit_str()? {
                parts.push(Part::Lit(s));
            } else if self.is_group(Delimiter::Parenthesis) {
                parts.push(Part::Value(
                    self.group(Delimiter::Parenthesis)?.whole_path()?,
                ));
            } else {
                return self.error("expected a string or a (name)");
            }
        }
        Ok(parts)
    }

    fn nodes(&mut self) -> Result<Vec<Node>> {
        let mut nodes = Vec::new();
        while self.peek().is_some() {
            nodes.push(self.node()?);
        }
        Ok(nodes)
    }

    fn block(&mut self) -> Result<Vec<Node>> {
        self.group(Delimiter::Brace)?.nodes()
    }

    fn node(&mut self) -> Result<Node> {
        if let Some(s) = self.lit_str()? {
            return Ok(Node::Text(s));
        }
        if self.is_group(Delimiter::Parenthesis) {
            return Ok(Node::Value(
                self.group(Delimiter::Parenthesis)?.whole_path()?,
            ));
        }
        if self.is_punct('@') {
            self.at += 1;
            if self.is_ident("if") {
                self.at += 1;
                return self.if_rest();
            }
            if self.is_ident("for") {
                self.at += 1;
                let bind = self.ident()?;
                if !self.is_ident("in") {
                    return self.error("expected `in`");
                }
                self.at += 1;
                let path = self.path()?;
                let body = self.block()?;
                return Ok(Node::For { bind, path, body });
            }
            return self.error("expected `@if` or `@for`");
        }
        self.element()
    }

    /// After `@if`: the condition or `let Some(x) = path`, the block, and
    /// any `@else if` / `@else`.
    fn if_rest(&mut self) -> Result<Node> {
        let node = if self.is_ident("let") {
            self.at += 1;
            if !self.is_ident("Some") {
                return self.error("only `@if let Some(name) = path`");
            }
            self.at += 1;
            let bind = self.group(Delimiter::Parenthesis)?.ident()?;
            self.expect_punct('=')?;
            let path = self.path()?;
            let then = self.block()?;
            let otherwise = self.otherwise()?;
            Node::IfLet {
                bind,
                path,
                then,
                otherwise,
            }
        } else {
            let cond = self.cond()?;
            let then = self.block()?;
            let otherwise = self.otherwise()?;
            Node::If {
                cond,
                then,
                otherwise,
            }
        };
        Ok(node)
    }

    fn otherwise(&mut self) -> Result<Vec<Node>> {
        if self.is_punct('@') && matches!(self.peek_at(1), Some(TokenTree::Ident(i)) if i == "else")
        {
            self.at += 2;
            if self.is_ident("if") {
                self.at += 1;
                return Ok(vec![self.if_rest()?]);
            }
            return self.block();
        }
        Ok(Vec::new())
    }

    fn element(&mut self) -> Result<Node> {
        let tag = self.hyphenated()?;
        let mut attrs = Vec::new();
        let mut spread = None;
        loop {
            if self.is_group(Delimiter::Brace) {
                let children = self.block()?;
                return Ok(Node::Element {
                    tag,
                    attrs,
                    spread,
                    children: Some(children),
                });
            }
            if self.is_punct(';') {
                self.at += 1;
                return Ok(Node::Element {
                    tag,
                    attrs,
                    spread,
                    children: None,
                });
            }
            if self.is_punct('.') && self.is_punct_at(1, '.') {
                self.at += 2;
                if spread.is_some() {
                    return self.error("one spread per element");
                }
                spread = Some(self.path()?);
                continue;
            }
            if self.peek().is_none() {
                return self.error("expected `{ ... }` or `;` to end the element");
            }
            attrs.push(self.attr()?);
        }
    }

    fn attr(&mut self) -> Result<Attr> {
        let name = self.hyphenated()?;
        if self.is_group(Delimiter::Bracket) {
            let mut inner = self.group(Delimiter::Bracket)?;
            let cond = inner.cond()?;
            if inner.peek().is_some() {
                return inner.error("expected only a condition");
            }
            return Ok(Attr::Bool(name, cond));
        }
        if !self.is_punct('=') {
            return Ok(Attr::Flag(name));
        }
        self.at += 1;
        if let Some(value) = self.lit_str()? {
            if self.is_ident("if") {
                self.at += 1;
                let cond = self.cond()?;
                return Ok(Attr::When(name, value, cond));
            }
            return Ok(Attr::Static(name, value));
        }
        if self.is_group(Delimiter::Parenthesis) {
            let path = self.group(Delimiter::Parenthesis)?.whole_path()?;
            return Ok(Attr::Value(name, path));
        }
        if self.is_group(Delimiter::Brace) {
            let parts = self.group(Delimiter::Brace)?.parts()?;
            return Ok(Attr::Concat(name, parts));
        }
        if self.is_group(Delimiter::Bracket) {
            let path = self.group(Delimiter::Bracket)?.whole_path()?;
            return Ok(Attr::Optional(name, path));
        }
        if self.is_ident("join") {
            self.at += 1;
            let mut inner = self.group(Delimiter::Bracket)?;
            let mut tokens = Vec::new();
            while inner.peek().is_some() {
                let parts = if let Some(s) = inner.lit_str()? {
                    vec![Part::Lit(s)]
                } else if inner.is_group(Delimiter::Brace) {
                    inner.group(Delimiter::Brace)?.parts()?
                } else {
                    return inner.error("expected a string or `{ ... }`");
                };
                let cond = if inner.is_ident("if") {
                    inner.at += 1;
                    Some(inner.cond()?)
                } else {
                    None
                };
                tokens.push((parts, cond));
                if inner.is_punct(',') {
                    inner.at += 1;
                } else if inner.peek().is_some() {
                    return inner.error("expected `,`");
                }
            }
            return Ok(Attr::Tokens(name, tokens));
        }
        self.error("expected \"value\", (name), { parts }, [name] or join[ ... ]")
    }
}
