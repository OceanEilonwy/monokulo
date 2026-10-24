//! Macros for monokulo's machine API (`monokulo::api`); see that module for
//! how a resource is written. Generated code refers to the runtime through
//! `crate::api::__private`, so these are for monokulo only.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as Tokens};
use quote::{format_ident, quote, quote_spanned};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{
    Attribute, Data, DeriveInput, Error, Expr, ExprLit, Fields, FnArg, GenericArgument, Ident,
    ItemFn, ItemStruct, Lit, LitInt, LitStr, Meta, Pat, PathArguments, ReturnType, Token, Type,
};

/// The runtime the generated code uses.
fn runtime() -> Tokens {
    quote!(crate::api::__private)
}

/// The text of `attrs`' doc comments: their lines, each without the space
/// `///` leaves, trimmed. `None` when there's none, or only blank ones.
fn doc_of(attrs: &[Attribute]) -> Option<String> {
    let mut lines = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        if let Meta::NameValue(nv) = &attr.meta {
            if let Expr::Lit(ExprLit {
                lit: Lit::Str(s), ..
            }) = &nv.value
            {
                let value = s.value();
                lines.push(value.strip_prefix(' ').unwrap_or(&value).to_owned());
            }
        }
    }
    let text = lines.join("\n").trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// The doc comment `attrs` must have, or a compile error at `span`.
fn require_doc(attrs: &[Attribute], span: Span, what: &str) -> syn::Result<String> {
    doc_of(attrs).ok_or_else(|| {
        Error::new(
            span,
            format!("describe this {what} with a doc comment (`/// ...`): it becomes the API docs"),
        )
    })
}

// -- resource! ---------------------------------------------------------------

struct ResourceArgs {
    path: LitStr,
    rel: LitStr,
    params: Vec<(Ident, Type)>,
    kind: Ident,
    doc: Option<LitStr>,
    /// For a page: who a link to it is offered to.
    who: Vec<Ident>,
}

impl Parse for ResourceArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let path: LitStr = input.parse()?;
        let mut rel = None;
        let mut params = Vec::new();
        let mut kind = Ident::new("hal", Span::call_site());
        let mut doc = None;
        let mut who = Vec::new();
        while !input.is_empty() {
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                break;
            }
            let key: Ident = input.parse()?;
            match key.to_string().as_str() {
                "rel" => {
                    input.parse::<Token![=]>()?;
                    rel = Some(input.parse::<LitStr>()?);
                }
                "kind" => {
                    input.parse::<Token![=]>()?;
                    kind = input.parse()?;
                    if !["hal", "stream", "page", "document"].contains(&kind.to_string().as_str()) {
                        return Err(Error::new(kind.span(), "kind is `hal`, `stream`, `page` or `document`"));
                    }
                }
                "doc" => {
                    input.parse::<Token![=]>()?;
                    doc = Some(input.parse::<LitStr>()?);
                }
                "who" => {
                    input.parse::<Token![=]>()?;
                    who.push(input.parse::<Ident>()?);
                    while input.peek(Token![|]) {
                        input.parse::<Token![|]>()?;
                        who.push(input.parse()?);
                    }
                }
                "params" => {
                    let content;
                    syn::parenthesized!(content in input);
                    let list = Punctuated::<ParamDecl, Token![,]>::parse_terminated(&content)?;
                    params = list.into_iter().map(|p| (p.name, p.ty)).collect();
                }
                _ => {
                    return Err(Error::new(
                        key.span(),
                        "expected `rel = \"...\"`, `params(name: Type, ...)`, `kind = ...`, `doc = \"...\"` or `who = ...`",
                    ))
                }
            }
        }
        let rel = rel.ok_or_else(|| Error::new(path.span(), "a resource needs `rel = \"...\"`"))?;
        if let Some(first) = who.first() {
            if kind != "page" {
                return Err(Error::new(
                    first.span(),
                    "only a page says who it's offered to; a resource's `GET` says who may read it",
                ));
            }
        }
        Ok(ResourceArgs {
            path,
            rel,
            params,
            kind,
            doc,
            who,
        })
    }
}

struct ParamDecl {
    name: Ident,
    ty: Type,
}

impl Parse for ParamDecl {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name = input.parse()?;
        input.parse::<Token![:]>()?;
        Ok(ParamDecl {
            name,
            ty: input.parse()?,
        })
    }
}

/// The literal segments and `{param}` names of a route template.
enum Segment {
    Text(String),
    Param(String),
}

fn segments(template: &str) -> Result<Vec<Segment>, String> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        if open > 0 {
            out.push(Segment::Text(rest[..open].to_owned()));
        }
        let close = rest[open..]
            .find('}')
            .ok_or_else(|| "an unclosed `{` in the path".to_owned())?;
        out.push(Segment::Param(rest[open + 1..open + close].to_owned()));
        rest = &rest[open + close + 1..];
    }
    if !rest.is_empty() {
        out.push(Segment::Text(rest.to_owned()));
    }
    Ok(out)
}

/// `api::resource!("/api/v1/orders/{order_id}", rel = "order",
/// params(order_id: OrderId))`: this file's resource. See `monokulo::api`.
#[proc_macro]
pub fn resource(input: TokenStream) -> TokenStream {
    let args = syn::parse_macro_input!(input as ResourceArgs);
    expand_resource(args)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

fn expand_resource(args: ResourceArgs) -> syn::Result<Tokens> {
    let p = runtime();
    let template = args.path.value();
    if !template.starts_with('/') {
        return Err(Error::new(args.path.span(), "a path starts with `/`"));
    }
    let parts = segments(&template).map_err(|e| Error::new(args.path.span(), e))?;
    let names: Vec<String> = parts
        .iter()
        .filter_map(|s| match s {
            Segment::Param(name) => Some(name.clone()),
            Segment::Text(_) => None,
        })
        .collect();
    for name in &names {
        if !args.params.iter().any(|(ident, _)| ident == name) {
            return Err(Error::new(
                args.path.span(),
                format!("give `{{{name}}}` a type: `params({name}: Type)`"),
            ));
        }
    }
    for (ident, _) in &args.params {
        if !names.contains(&ident.to_string()) {
            return Err(Error::new(
                ident.span(),
                format!("`{ident}` isn't in the path"),
            ));
        }
    }
    let rel = &args.rel;
    let rel_value = rel.value();
    if rel_value.is_empty()
        || !rel_value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '-' || c.is_ascii_digit())
    {
        return Err(Error::new(
            rel.span(),
            "a rel is lowercase letters, digits and `-`",
        ));
    }
    let kind = match args.kind.to_string().as_str() {
        "stream" => quote!(#p::Kind::Stream),
        "page" => quote!(#p::Kind::Page),
        "document" => quote!(#p::Kind::Document),
        _ => quote!(#p::Kind::Hal),
    };
    let doc = match &args.doc {
        Some(doc) => quote!(::core::option::Option::Some(#doc)),
        None => quote!(::core::option::Option::None),
    };
    let who = if args.who.is_empty() {
        quote!(::core::option::Option::None)
    } else {
        let bits = who_bits(&args.who, args.rel.span())?;
        quote!(::core::option::Option::Some(#p::Who(#bits)))
    };
    let field_names: Vec<&Ident> = args.params.iter().map(|(n, _)| n).collect();
    let field_types: Vec<&Type> = args.params.iter().map(|(_, t)| t).collect();
    let writes = parts.iter().map(|segment| match segment {
        Segment::Text(text) => quote!(f.write_str(#text)?;),
        Segment::Param(name) => {
            let ident = format_ident!("{}", name);
            quote!(f.write_str(&#p::encode_segment(&self.#ident.to_string()))?;)
        }
    });
    let param_names: Vec<&String> = names.iter().collect();
    let path_doc = format!("This file's resource, at `{template}`.");
    let extract = if args.params.is_empty() {
        quote!(::core::result::Result::Ok(Path {}))
    } else {
        quote! {
            match <#p::axum::extract::Path<Path> as #p::axum::extract::FromRequestParts<S>>::from_request_parts(parts, state).await {
                ::core::result::Result::Ok(#p::axum::extract::Path(path)) => ::core::result::Result::Ok(path),
                ::core::result::Result::Err(_) => ::core::result::Result::Err(#p::not_found()),
            }
        }
    };
    // A path that doesn't parse (an order id that can't be one) is
    // `not-found`.
    let path_problems = if args.params.is_empty() {
        quote!()
    } else {
        quote!(operation.problem(#p::Problem::NotFound);)
    };
    Ok(quote! {
        /// One resource per file: a second `resource!` here defines this
        /// twice.
        #[allow(dead_code, non_upper_case_globals)]
        const ONLY_ONE_API_RESOURCE_PER_FILE: () = ();

        #[doc = #path_doc]
        #[derive(Clone, Debug, PartialEq, Eq, #p::serde::Deserialize)]
        #[serde(crate = "crate::api::__private::serde")]
        pub struct Path {
            #(pub #field_names: #field_types,)*
        }

        impl Path {
            /// The route's template, as the router registers it.
            pub const TEMPLATE: &'static str = #template;
        }

        impl ::core::fmt::Display for Path {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                #(#writes)*
                ::core::result::Result::Ok(())
            }
        }

        #[#p::linkme::distributed_slice(#p::RESOURCES)]
        #[linkme(crate = crate::api::__private::linkme)]
        static API_RESOURCE: #p::ResourceInfo = #p::ResourceInfo {
            template: #template,
            rel: #rel,
            kind: #kind,
            doc: #doc,
            params: &[#(#param_names),*],
            who: #who,
            file: ::core::file!(),
            module: ::core::module_path!(),
        };

        /// The body's own path: its fields named after the path's
        /// parameters (`#[api::body]` uses it for `self`).
        #[allow(unused_macros)]
        macro_rules! api_self_path {
            ($body:expr) => {
                Path { #(#field_names: ::core::clone::Clone::clone(&$body.#field_names),)* }
            };
        }

        impl #p::Resource for Path {
            fn info() -> &'static #p::ResourceInfo {
                &API_RESOURCE
            }
        }

        impl<S: ::core::marker::Send + ::core::marker::Sync> #p::axum::extract::FromRequestParts<S> for Path {
            type Rejection = #p::ProblemDetails;

            async fn from_request_parts(
                parts: &mut #p::axum::http::request::Parts,
                state: &S,
            ) -> ::core::result::Result<Self, Self::Rejection> {
                let _ = (&parts, &state);
                #extract
            }
        }

        impl #p::ApiParam for Path {
            fn describe(operation: &mut #p::OperationDoc) {
                operation.path_params(&[#(#param_names),*]);
                #path_problems
            }
        }
    })
}

// -- #[get] / #[post] / #[delete] --------------------------------------------

struct HandlerArgs {
    who: Vec<Ident>,
    form: Option<LitStr>,
    /// The problems the handler's own body answers with (`Problem`
    /// variants), beyond its parameters' and the middleware's.
    problems: Vec<Ident>,
}

impl Parse for HandlerArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut who = Vec::new();
        let mut form = None;
        let mut problems = Vec::new();
        while !input.is_empty() {
            if input.peek(Ident) && input.peek2(Token![=]) {
                let key: Ident = input.parse()?;
                input.parse::<Token![=]>()?;
                if key == "form" {
                    form = Some(input.parse()?);
                } else if key == "problems" {
                    let list;
                    syn::bracketed!(list in input);
                    let names =
                        syn::punctuated::Punctuated::<Ident, Token![,]>::parse_terminated(&list)?;
                    problems.extend(names);
                } else {
                    return Err(Error::new(
                        key.span(),
                        "expected `form = \"name\"` or `problems = [Variant, ...]`",
                    ));
                }
            } else {
                who.push(input.parse::<Ident>()?);
                while input.peek(Token![|]) {
                    input.parse::<Token![|]>()?;
                    who.push(input.parse()?);
                }
            }
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(HandlerArgs {
            who,
            form,
            problems,
        })
    }
}

/// The bits of `who`: see `monokulo::api::Who`.
fn who_bits(who: &[Ident], span: Span) -> syn::Result<u8> {
    const NONE: u8 = 1;
    const PK: u8 = 2;
    const SK: u8 = 4;
    const AK: u8 = 8;
    const SESSION: u8 = 16;
    if who.is_empty() {
        return Err(Error::new(
            span,
            "say who may call this: `pk`, `sk`, `ak`, `session`, `any_key`, `server`, `app` or `anyone`",
        ));
    }
    let mut bits = 0;
    for ident in who {
        bits |=
            match ident.to_string().as_str() {
                "pk" => PK,
                "sk" => SK,
                "ak" => AK,
                "session" => SESSION,
                "any_key" => PK | SK | AK,
                "server" => SK | AK,
                "app" => AK,
                "anyone" => NONE | PK | SK | AK | SESSION,
                _ => return Err(Error::new(
                    ident.span(),
                    "expected `pk`, `sk`, `ak`, `session`, `any_key`, `server`, `app` or `anyone`",
                )),
            };
    }
    Ok(bits)
}

/// The `T` of a handler's `api::Result<T>`.
fn result_type(output: &ReturnType) -> syn::Result<Type> {
    let ReturnType::Type(_, ty) = output else {
        return Err(Error::new(
            output.span(),
            "a handler returns `api::Result<T>`",
        ));
    };
    if let Type::Path(path) = ty.as_ref() {
        if let Some(last) = path.path.segments.last() {
            if last.ident == "Result" {
                if let PathArguments::AngleBracketed(args) = &last.arguments {
                    if let Some(GenericArgument::Type(t)) = args.args.first() {
                        return Ok(t.clone());
                    }
                }
            }
        }
    }
    Err(Error::new(ty.span(), "a handler returns `api::Result<T>`"))
}

fn handler(method: &str, attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = syn::parse_macro_input!(attr as HandlerArgs);
    let function = syn::parse_macro_input!(item as ItemFn);
    expand_handler(method, args, function)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

fn expand_handler(method: &str, args: HandlerArgs, function: ItemFn) -> syn::Result<Tokens> {
    let p = runtime();
    let name = &function.sig.ident;
    let doc = require_doc(&function.attrs, name.span(), "handler")?;
    if function.sig.asyncness.is_none() {
        return Err(Error::new(
            function.sig.fn_token.span(),
            "a handler is an `async fn`",
        ));
    }
    let bits = who_bits(&args.who, name.span())?;
    let output = result_type(&function.sig.output)?;
    let mut types = Vec::new();
    for input in &function.sig.inputs {
        match input {
            FnArg::Typed(typed) => {
                if !matches!(
                    typed.pat.as_ref(),
                    Pat::Ident(_) | Pat::Wild(_) | Pat::TupleStruct(_) | Pat::Struct(_)
                ) {
                    return Err(Error::new(typed.pat.span(), "name each parameter"));
                }
                types.push((*typed.ty).clone());
            }
            FnArg::Receiver(receiver) => {
                return Err(Error::new(receiver.span(), "a handler is a free function"))
            }
        }
    }
    let args_idents: Vec<Ident> = (0..types.len())
        .map(|i| format_ident!("arg{}", i))
        .collect();
    let info = format_ident!("API_HANDLER_{}", name.to_string().to_uppercase());
    let route = format_ident!("api_route_{}", name);
    let describe = format_ident!("api_describe_{}", name);
    let wrapper = format_ident!("api_handler_{}", name);
    let marker_who = format_ident!("ApiWho_{}", name);
    let routing = format_ident!("{}", method.to_ascii_lowercase());
    let form = match &args.form {
        Some(form) => {
            let value = form.value();
            if value.is_empty()
                || !value
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '-' || c.is_ascii_digit())
            {
                return Err(Error::new(
                    form.span(),
                    "a form's name is lowercase letters, digits and `-`",
                ));
            }
            if method == "GET" {
                return Err(Error::new(
                    form.span(),
                    "a form changes something: it's a `post` or `delete`",
                ));
            }
            quote!(::core::option::Option::Some(#form))
        }
        None => quote!(::core::option::Option::None),
    };
    let name_text = name.to_string();
    let output_span = output.span();
    let respond_check =
        quote_spanned!(output_span=> <#output as #p::Respond>::describe(operation););
    // Each a `Problem` variant, checked by the compiler where it's named.
    let problems = args
        .problems
        .iter()
        .map(|problem| quote_spanned!(problem.span()=> operation.problem(#p::Problem::#problem);));
    Ok(quote! {
        #function

        /// This handler as a form target (`api::Form<...>`).
        #[allow(non_camel_case_types, dead_code)]
        pub(crate) struct #name {}

        impl #p::FormTarget for #name {
            fn info() -> &'static #p::HandlerInfo {
                &#info
            }
        }

        #[allow(non_camel_case_types)]
        struct #marker_who;

        impl #p::WhoMarker for #marker_who {
            const WHO: #p::Who = #p::Who(#bits);
        }

        #[allow(non_snake_case)]
        async fn #wrapper(
            authorized: #p::Authorized<#marker_who>,
            #(#args_idents: #types,)*
        ) -> #p::axum::response::Response {
            #p::respond(authorized, #name(#(#args_idents),*)).await
        }

        fn #route(router: #p::axum::Router<#p::AppState>) -> #p::axum::Router<#p::AppState> {
            router.route(Path::TEMPLATE, #p::axum::routing::#routing(#wrapper))
        }

        fn #describe(operation: &mut #p::OperationDoc) {
            #(<#types as #p::ApiParam>::describe(operation);)*
            #respond_check
            #(#problems)*
        }

        #[#p::linkme::distributed_slice(#p::HANDLERS)]
        #[linkme(crate = crate::api::__private::linkme)]
        #[allow(non_upper_case_globals)]
        static #info: #p::HandlerInfo = #p::HandlerInfo {
            template: Path::TEMPLATE,
            method: #method,
            name: #name_text,
            who: #p::Who(#bits),
            form: #form,
            doc: #doc,
            file: ::core::file!(),
            module: ::core::module_path!(),
            route: #route,
            describe: #describe,
        };
    })
}

/// `#[get(who, problems = [Variant, ...])]`: this resource's `GET`. See
/// `monokulo::api`.
#[proc_macro_attribute]
pub fn get(attr: TokenStream, item: TokenStream) -> TokenStream {
    handler("GET", attr, item)
}

/// `#[post(who, form = "name", problems = [Variant, ...])]`: this
/// resource's `POST`.
#[proc_macro_attribute]
pub fn post(attr: TokenStream, item: TokenStream) -> TokenStream {
    handler("POST", attr, item)
}

/// `#[delete(who, form = "name", problems = [Variant, ...])]`: this
/// resource's `DELETE`.
#[proc_macro_attribute]
pub fn delete(attr: TokenStream, item: TokenStream) -> TokenStream {
    handler("DELETE", attr, item)
}

// -- #[body] / #[object] / #[input] ------------------------------------------

/// What a field of a body is.
enum FieldKind {
    Data,
    /// `Link<T>`: required unless `optional`.
    Link {
        target: Type,
        optional: bool,
    },
    /// `Form<H>`.
    Form {
        target: Type,
    },
    /// `Href`: a link outside the API, under a registered relation.
    Href,
    /// `Option<OnionLink<T>>`: the resource at `T`, absolute, on the
    /// instance's onion address; its own relation (`mk:onion-…`), which
    /// the field's doc comment describes.
    OnionLink {
        target: Type,
    },
    /// `OnionLink<T>` that isn't an `Option`: refused, since an instance
    /// with no onion address has no such link.
    BareOnionLink,
}

/// The `T` in `Name<T>`, when `ty` is that.
fn generic_of<'a>(ty: &'a Type, name: &str) -> Option<&'a Type> {
    let Type::Path(path) = ty else { return None };
    let last = path.path.segments.last()?;
    if last.ident != name {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &last.arguments else {
        return None;
    };
    match args.args.first()? {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    }
}

fn field_kind(ty: &Type) -> FieldKind {
    if matches!(ty, Type::Path(path) if path.path.is_ident("Href")) {
        return FieldKind::Href;
    }
    if let Some(target) = generic_of(ty, "Link") {
        return FieldKind::Link {
            target: target.clone(),
            optional: false,
        };
    }
    if generic_of(ty, "OnionLink").is_some() {
        return FieldKind::BareOnionLink;
    }
    if let Some(inner) = generic_of(ty, "Option") {
        if let Some(target) = generic_of(inner, "Link") {
            return FieldKind::Link {
                target: target.clone(),
                optional: true,
            };
        }
        if let Some(target) = generic_of(inner, "OnionLink") {
            return FieldKind::OnionLink {
                target: target.clone(),
            };
        }
    }
    if let Some(target) = generic_of(ty, "Form") {
        return FieldKind::Form {
            target: target.clone(),
        };
    }
    FieldKind::Data
}

/// Links named after a registered relation keep its name (`next`).
const REGISTERED_RELS: &[&str] = &[
    "next",
    "prev",
    "first",
    "last",
    "collection",
    "item",
    "up",
    "related",
    "alternate",
    "service-desc",
    "service-doc",
    "describedby",
];

fn named_fields(item: &ItemStruct) -> syn::Result<Vec<&syn::Field>> {
    match &item.fields {
        Fields::Named(fields) => Ok(fields.named.iter().collect()),
        _ => Err(Error::new(item.ident.span(), "use named fields")),
    }
}

/// `#[api::body]`: a resource's body. See `monokulo::api`.
#[proc_macro_attribute]
pub fn body(attr: TokenStream, item: TokenStream) -> TokenStream {
    let _ = attr;
    let item = syn::parse_macro_input!(item as ItemStruct);
    expand_body(item, true)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// `#[api::object]`: a part of a body (a nested object). Like a body, but
/// with no links of its own.
#[proc_macro_attribute]
pub fn object(attr: TokenStream, item: TokenStream) -> TokenStream {
    let _ = attr;
    let item = syn::parse_macro_input!(item as ItemStruct);
    expand_body(item, false)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

fn expand_body(item: ItemStruct, resource: bool) -> syn::Result<Tokens> {
    let p = runtime();
    let what = if resource { "body" } else { "object" };
    let doc = require_doc(&item.attrs, item.ident.span(), what)?;
    let name = &item.ident;
    if !item.generics.params.is_empty() {
        return Err(Error::new(item.generics.span(), "a body has no generics"));
    }
    let fields = named_fields(&item)?;
    let mut data = Vec::new();
    let mut links = Vec::new();
    let mut forms = Vec::new();
    let mut hrefs = Vec::new();
    let mut onions = Vec::new();
    for field in &fields {
        let ident = field.ident.clone().expect("named");
        match field_kind(&field.ty) {
            FieldKind::Data => {
                let doc = require_doc(&field.attrs, ident.span(), "field")?;
                data.push((ident, field.ty.clone(), doc, field));
            }
            FieldKind::Link { target, optional } => links.push((ident, target, optional)),
            FieldKind::Form { target } => {
                if !resource {
                    return Err(Error::new(
                        field.ty.span(),
                        "forms go on the resource's `#[api::body]`",
                    ));
                }
                forms.push((ident, target));
            }
            FieldKind::Href => {
                let rel = ident.to_string().replace('_', "-");
                if !resource || !REGISTERED_RELS.contains(&rel.as_str()) {
                    return Err(Error::new(
                        ident.span(),
                        "an `Href` is a link outside the API under a registered relation (`service_doc`)",
                    ));
                }
                hrefs.push((ident, rel));
            }
            FieldKind::BareOnionLink => {
                return Err(Error::new(
                    field.ty.span(),
                    "an onion link is an `Option<OnionLink<T>>`: absent while the instance has no onion address",
                ));
            }
            FieldKind::OnionLink { target } => {
                let rel = ident.to_string().replace('_', "-");
                if !resource || !rel.starts_with("onion-") {
                    return Err(Error::new(
                        ident.span(),
                        "an `OnionLink` goes on a resource's `#[api::body]`, in a field named `onion_…`",
                    ));
                }
                let doc = require_doc(
                    &field.attrs,
                    ident.span(),
                    "onion link (its relation's page)",
                )?;
                onions.push((ident, target, rel, doc));
            }
        }
    }
    let struct_attrs = &item.attrs;
    let all_fields = fields.iter().map(|field| {
        let attrs = &field.attrs;
        let ident = &field.ident;
        let ty = &field.ty;
        quote!(#(#attrs)* pub(crate) #ident: #ty,)
    });
    let data_idents: Vec<&Ident> = data.iter().map(|(i, ..)| i).collect();
    let data_names: Vec<String> = data_idents.iter().map(|i| i.to_string()).collect();
    let data_types: Vec<&Type> = data.iter().map(|(_, t, ..)| t).collect();
    let data_docs: Vec<&String> = data.iter().map(|(_, _, d, _)| d).collect();
    // A field with no value is left out, never sent as `null`: an
    // `Option<T>` field is written only when it has one, and its schema is
    // `T`'s, not required.
    let serialize_data: Vec<Tokens> = data_idents
        .iter()
        .zip(&data_names)
        .zip(&data_types)
        .map(|((ident, name), ty)| {
            if generic_of(ty, "Option").is_some() {
                quote! {
                    if let ::core::option::Option::Some(value) = &self.#ident {
                        map.serialize_entry(#name, value)?;
                    }
                }
            } else {
                quote!(map.serialize_entry(#name, &self.#ident)?;)
            }
        })
        .collect();
    let schema_data: Vec<Tokens> = data_names
        .iter()
        .zip(&data_types)
        .zip(&data_docs)
        .map(|((name, ty), doc)| match generic_of(ty, "Option") {
            Some(inner) => quote! {
                properties.insert(#name.into(), #p::described(generator.subschema_for::<#inner>(), #doc));
            },
            None => quote! {
                properties.insert(#name.into(), #p::described(generator.subschema_for::<#ty>(), #doc));
                required.push(#name);
            },
        })
        .collect();
    let link_adds = links.iter().map(|(ident, target, optional)| {
        let rel = ident.to_string().replace('_', "-");
        let registered = REGISTERED_RELS.contains(&rel.as_str());
        let add = quote!(links.add::<#target>(link, #registered.then_some(#rel)););
        if *optional {
            quote!(if let ::core::option::Option::Some(link) = &self.#ident { #add })
        } else {
            quote!({ let link = &self.#ident; #add })
        }
    });
    let link_schemas = links.iter().map(|(ident, target, _)| {
        let rel = ident.to_string().replace('_', "-");
        let registered = REGISTERED_RELS.contains(&rel.as_str());
        quote!(links.push((#p::link_key::<#target>(#registered.then_some(#rel)), #p::link_title::<#target>()));)
    });
    let href_adds = hrefs
        .iter()
        .map(|(ident, rel)| quote!(links.add_href(&self.#ident, #rel);));
    let href_schemas = hrefs
        .iter()
        .map(|(_, rel)| quote!(links.push((::std::string::String::from(#rel), ::std::string::String::new()));));
    let form_adds = forms
        .iter()
        .map(|(_, target)| quote!(templates.add::<#target>();));
    let onion_adds = onions.iter().map(|(ident, target, rel, doc)| {
        let key = format!("mk:{rel}");
        let title = summary(doc);
        quote!(if let ::core::option::Option::Some(link) = &self.#ident { links.add_onion::<#target>(link, #key, #title); })
    });
    let onion_schemas = onions.iter().map(|(_, _, rel, doc)| {
        let key = format!("mk:{rel}");
        let title = summary(doc);
        quote!(links.push((::std::string::String::from(#key), ::std::string::String::from(#title)));)
    });
    let onion_registers = onions.iter().map(|(ident, target, rel, doc)| {
        let info = format_ident!(
            "API_REL_{}_{}",
            name.to_string().to_uppercase(),
            ident.to_string().to_uppercase()
        );
        quote! {
            #[#p::linkme::distributed_slice(#p::RELS)]
            #[linkme(crate = crate::api::__private::linkme)]
            #[allow(non_upper_case_globals)]
            static #info: #p::RelInfo = #p::RelInfo {
                rel: #rel,
                doc: #doc,
                target: <#target as #p::Resource>::info,
                file: ::core::file!(),
            };
        }
    });
    let name_text = name.to_string();
    let register = if resource {
        let info = format_ident!("API_BODY_{}", name.to_string().to_uppercase());
        quote! {
            #[#p::linkme::distributed_slice(#p::BODIES)]
            #[linkme(crate = crate::api::__private::linkme)]
            #[allow(non_upper_case_globals)]
            static #info: #p::BodyInfo = #p::BodyInfo {
                name: #name_text,
                doc: #doc,
                file: ::core::file!(),
                module: ::core::module_path!(),
                schema: <#name as #p::schemars::JsonSchema>::json_schema,
            };

            impl #p::Body for #name {
                type Path = Path;
                fn self_path(&self) -> Path {
                    api_self_path!(self)
                }
            }

            #(#onion_registers)*
        }
    } else {
        quote!()
    };
    let serialize_links = if resource {
        quote! {
            let mut links = #p::LinksBuilder::new(&<#name as #p::Body>::self_path(self));
            #(#link_adds)*
            #(#onion_adds)*
            #(#href_adds)*
            map.serialize_entry("_links", &links)?;
            let mut templates = #p::TemplatesBuilder::for_resource::<Path>();
            #(#form_adds)*
            if !templates.is_empty() {
                map.serialize_entry("_templates", &templates)?;
            }
        }
    } else if !links.is_empty() {
        // A part of a body with links: `_links`, without `self`.
        quote! {
            let mut links = #p::LinksBuilder::without_self();
            #(#link_adds)*
            map.serialize_entry("_links", &links)?;
        }
    } else {
        quote!()
    };
    let schema_links = if resource {
        quote! {
            let mut links: ::std::vec::Vec<(::std::string::String, ::std::string::String)> = ::std::vec::Vec::new();
            #(#link_schemas)*
            #(#onion_schemas)*
            #(#href_schemas)*
            #p::hal_parts(&mut properties, links);
        }
    } else if !links.is_empty() {
        quote! {
            let mut links: ::std::vec::Vec<(::std::string::String, ::std::string::String)> = ::std::vec::Vec::new();
            #(#link_schemas)*
            #p::object_links(&mut properties, links);
        }
    } else {
        quote!()
    };
    Ok(quote! {
        #(#struct_attrs)*
        pub(crate) struct #name {
            #(#all_fields)*
        }

        impl #p::serde::Serialize for #name {
            fn serialize<S: #p::serde::Serializer>(&self, serializer: S) -> ::core::result::Result<S::Ok, S::Error> {
                use #p::serde::ser::SerializeMap as _;
                let mut map = serializer.serialize_map(::core::option::Option::None)?;
                #(#serialize_data)*
                #serialize_links
                map.end()
            }
        }

        impl #p::schemars::JsonSchema for #name {
            fn schema_name() -> ::std::borrow::Cow<'static, str> {
                #p::schema_name(::core::module_path!(), #name_text).into()
            }

            fn json_schema(generator: &mut #p::schemars::SchemaGenerator) -> #p::schemars::Schema {
                let mut properties = #p::serde_json::Map::new();
                let mut required: ::std::vec::Vec<&str> = ::std::vec::Vec::new();
                #(#schema_data)*
                #schema_links
                #p::object_schema(#doc, properties, required)
            }
        }

        #register
    })
}

/// `#[api::input]`: what a form sends. See `monokulo::api`.
#[proc_macro_attribute]
pub fn input(attr: TokenStream, item: TokenStream) -> TokenStream {
    let _ = attr;
    let item = syn::parse_macro_input!(item as ItemStruct);
    expand_input(item)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

fn expand_input(item: ItemStruct) -> syn::Result<Tokens> {
    let p = runtime();
    require_doc(&item.attrs, item.ident.span(), "input")?;
    for field in named_fields(&item)? {
        let ident = field.ident.as_ref().expect("named");
        require_doc(&field.attrs, ident.span(), "field")?;
        if !matches!(field_kind(&field.ty), FieldKind::Data) {
            return Err(Error::new(
                field.ty.span(),
                "an input holds values, not links or forms",
            ));
        }
    }
    let name = &item.ident;
    let attrs = &item.attrs;
    let fields = item.fields.iter().map(|field| {
        let attrs = &field.attrs;
        let ident = &field.ident;
        let ty = &field.ty;
        quote!(#(#attrs)* pub(crate) #ident: #ty,)
    });
    let name_text = name.to_string();
    let doc = doc_of(&item.attrs).unwrap_or_default();
    let mut field_names = Vec::new();
    let mut field_types = Vec::new();
    let mut field_docs = Vec::new();
    let mut required = Vec::new();
    for field in &item.fields {
        let ident = field.ident.as_ref().expect("named");
        field_names.push(ident.to_string());
        field_types.push(field.ty.clone());
        field_docs.push(doc_of(&field.attrs).unwrap_or_default());
        if generic_of(&field.ty, "Option").is_none() {
            required.push(ident.to_string());
        }
    }
    Ok(quote! {
        #(#attrs)*
        #[derive(#p::serde::Deserialize)]
        #[serde(crate = "crate::api::__private::serde")]
        pub(crate) struct #name {
            #(#fields)*
        }

        impl #p::schemars::JsonSchema for #name {
            fn schema_name() -> ::std::borrow::Cow<'static, str> {
                #p::schema_name(::core::module_path!(), #name_text).into()
            }

            fn json_schema(generator: &mut #p::schemars::SchemaGenerator) -> #p::schemars::Schema {
                let mut properties = #p::serde_json::Map::new();
                #(
                    properties.insert(#field_names.into(), #p::described(generator.subschema_for::<#field_types>(), #field_docs));
                )*
                #p::object_schema(#doc, properties, ::std::vec![#(#required),*])
            }
        }

        impl<S: ::core::marker::Send + ::core::marker::Sync> #p::axum::extract::FromRequest<S> for #name {
            type Rejection = #p::ProblemDetails;

            async fn from_request(
                request: #p::axum::extract::Request,
                state: &S,
            ) -> ::core::result::Result<Self, Self::Rejection> {
                #p::json_body(request, state).await
            }
        }

        impl #p::ApiParam for #name {
            fn describe(operation: &mut #p::OperationDoc) {
                operation.body::<#name>();
                // `json_body`'s answers to a body it can't read.
                operation.problem(#p::Problem::InvalidRequest);
                operation.problem(#p::Problem::UnsupportedMediaType);
                operation.problem(#p::Problem::TooLarge);
            }
        }

        impl #p::Input for #name {}
    })
}

// -- #[scalar] ---------------------------------------------------------------

struct ScalarArgs {
    pattern: Option<LitStr>,
    example: Option<LitStr>,
    min_length: Option<LitInt>,
    max_length: Option<LitInt>,
    trim: bool,
}

impl Parse for ScalarArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut args = ScalarArgs {
            pattern: None,
            example: None,
            min_length: None,
            max_length: None,
            trim: false,
        };
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            match key.to_string().as_str() {
                "trim" => args.trim = true,
                "pattern" => {
                    input.parse::<Token![=]>()?;
                    args.pattern = Some(input.parse()?);
                }
                "example" => {
                    input.parse::<Token![=]>()?;
                    args.example = Some(input.parse()?);
                }
                "min_length" => {
                    input.parse::<Token![=]>()?;
                    args.min_length = Some(input.parse()?);
                }
                "max_length" => {
                    input.parse::<Token![=]>()?;
                    args.max_length = Some(input.parse()?);
                }
                _ => {
                    return Err(Error::new(
                        key.span(),
                        "expected `pattern`, `example`, `min_length`, `max_length` or `trim`",
                    ))
                }
            }
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(args)
    }
}

/// `#[api::scalar(pattern = r"...", example = "...", min_length = 1,
/// max_length = 120, trim)] pub struct Name(String);`: a string the API
/// takes or gives, checked when it's read and described in the schema.
#[proc_macro_attribute]
pub fn scalar(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = syn::parse_macro_input!(attr as ScalarArgs);
    let item = syn::parse_macro_input!(item as ItemStruct);
    expand_scalar(args, item)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

fn expand_scalar(args: ScalarArgs, item: ItemStruct) -> syn::Result<Tokens> {
    let p = runtime();
    let doc = require_doc(&item.attrs, item.ident.span(), "scalar")?;
    let ok_shape = matches!(&item.fields, Fields::Unnamed(f) if f.unnamed.len() == 1
        && matches!(&f.unnamed[0].ty, Type::Path(t) if t.path.is_ident("String")));
    if !ok_shape {
        return Err(Error::new(
            item.ident.span(),
            "a scalar is `struct Name(String);`",
        ));
    }
    let Some(example) = &args.example else {
        return Err(Error::new(
            item.ident.span(),
            "give the scalar an `example = \"...\"`: it's shown in the docs",
        ));
    };
    if let Some(pattern) = &args.pattern {
        if pattern.value().is_empty() {
            return Err(Error::new(pattern.span(), "an empty pattern"));
        }
    }
    let name = &item.ident;
    let vis = &item.vis;
    let attrs = &item.attrs;
    let name_text = name.to_string();
    let pattern = match &args.pattern {
        Some(p) => quote!(::core::option::Option::Some(#p)),
        None => quote!(::core::option::Option::None),
    };
    let min = match &args.min_length {
        Some(n) => quote!(::core::option::Option::Some(#n)),
        None => quote!(::core::option::Option::None),
    };
    let max = match &args.max_length {
        Some(n) => quote!(::core::option::Option::Some(#n)),
        None => quote!(::core::option::Option::None),
    };
    let trim = args.trim;
    let rules = format_ident!("{}_RULES", name_text.to_uppercase());
    Ok(quote! {
        #(#attrs)*
        #[derive(Clone, Debug, PartialEq, Eq, Hash)]
        #vis struct #name(::std::string::String);

        #[allow(non_upper_case_globals)]
        static #rules: #p::ScalarRules = #p::ScalarRules {
            name: #name_text,
            doc: #doc,
            pattern: #pattern,
            example: #example,
            min_length: #min,
            max_length: #max,
            trim: #trim,
            regex: ::std::sync::OnceLock::new(),
        };

        impl #name {
            /// `value`, if it follows this type's rules.
            pub fn parse(value: &str) -> ::core::result::Result<Self, ::std::string::String> {
                #rules.check(value).map(#name)
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> ::std::string::String {
                self.0
            }
        }

        impl ::core::fmt::Display for #name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl #p::serde::Serialize for #name {
            fn serialize<S: #p::serde::Serializer>(&self, serializer: S) -> ::core::result::Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> #p::serde::Deserialize<'de> for #name {
            fn deserialize<D: #p::serde::Deserializer<'de>>(deserializer: D) -> ::core::result::Result<Self, D::Error> {
                let value = <::std::string::String as #p::serde::Deserialize>::deserialize(deserializer)?;
                #name::parse(&value).map_err(<D::Error as #p::serde::de::Error>::custom)
            }
        }

        impl #p::schemars::JsonSchema for #name {
            fn schema_name() -> ::std::borrow::Cow<'static, str> {
                #name_text.into()
            }

            fn json_schema(_: &mut #p::schemars::SchemaGenerator) -> #p::schemars::Schema {
                #rules.schema()
            }
        }
    })
}

// -- #[derive(Problem)] ------------------------------------------------------

/// `#[derive(api::Problem)]` on the API's problem enum: each variant needs
/// a doc comment (its docs page) and `#[problem(status = 404, title =
/// "...")]`; its slug is its name in kebab case.
#[proc_macro_derive(Problem, attributes(problem))]
pub fn derive_problem(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    expand_problem(input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// The first paragraph of `doc`, on one line: a link's title.
fn summary(doc: &str) -> String {
    doc.split("\n\n")
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn kebab(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn expand_problem(input: DeriveInput) -> syn::Result<Tokens> {
    let Data::Enum(data) = &input.data else {
        return Err(Error::new(
            input.ident.span(),
            "`Problem` is derived on an enum",
        ));
    };
    let name = &input.ident;
    let mut variants = Vec::new();
    let mut slugs = Vec::new();
    let mut statuses = Vec::new();
    let mut titles = Vec::new();
    let mut docs = Vec::new();
    for variant in &data.variants {
        if !matches!(variant.fields, Fields::Unit) {
            return Err(Error::new(
                variant.span(),
                "a problem variant carries no data",
            ));
        }
        let doc = require_doc(&variant.attrs, variant.ident.span(), "problem")?;
        let attr = variant
            .attrs
            .iter()
            .find(|a| a.path().is_ident("problem"))
            .ok_or_else(|| {
                Error::new(
                    variant.ident.span(),
                    "add `#[problem(status = ..., title = \"...\")]`",
                )
            })?;
        let mut status: Option<LitInt> = None;
        let mut title: Option<LitStr> = None;
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("status") {
                status = Some(meta.value()?.parse()?);
                Ok(())
            } else if meta.path.is_ident("title") {
                title = Some(meta.value()?.parse()?);
                Ok(())
            } else {
                Err(meta.error("expected `status` or `title`"))
            }
        })?;
        let status =
            status.ok_or_else(|| Error::new(attr.span(), "give the problem a `status`"))?;
        let code: u16 = status.base10_parse()?;
        if !(400..600).contains(&code) {
            return Err(Error::new(
                status.span(),
                "a problem's status is 4xx or 5xx",
            ));
        }
        let title = title.ok_or_else(|| Error::new(attr.span(), "give the problem a `title`"))?;
        slugs.push(kebab(&variant.ident.to_string()));
        statuses.push(code);
        titles.push(title);
        docs.push(doc);
        variants.push(variant.ident.clone());
    }
    Ok(quote! {
        impl #name {
            /// Every problem.
            pub const ALL: &'static [#name] = &[#(#name::#variants),*];

            /// Its name in its `type` URI and its docs page.
            pub fn slug(self) -> &'static str {
                match self { #(#name::#variants => #slugs,)* }
            }

            /// Its HTTP status.
            pub fn status(self) -> crate::api::__private::axum::http::StatusCode {
                match self {
                    #(#name::#variants => crate::api::__private::axum::http::StatusCode::from_u16(#statuses).expect("a status"),)*
                }
            }

            /// A short summary, the same for every occurrence.
            pub fn title(self) -> &'static str {
                match self { #(#name::#variants => #titles,)* }
            }

            /// What its docs page says.
            pub fn doc(self) -> &'static str {
                match self { #(#name::#variants => #docs,)* }
            }
        }
    })
}
