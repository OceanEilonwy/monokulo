//! `bake!`: a file's bytes and its compressed copies, made when the crate
//! that lists the file is compiled (`static_assets::Baked`).
//!
//! - `bake!("static/", "app.js")`: the file at those pieces joined, under
//!   the calling crate's directory;
//! - `bake!(out_dir "/web/app.js")`: a file its build script made;
//! - `bake!(join "\n" ["a.css", "b.css"])`: several files under the crate's
//!   directory, one after another with the separator between them.
//!
//! Scripts, styles, SVG, WebAssembly, JSON and text get a brotli and a gzip
//! copy, each kept only if it's smaller; other files (fonts, images) are
//! compressed already. The copies are kept on disk by content
//! (`static-assets-cache/` beside the build's `build/` folder), so a
//! rebuild that changed none of a file doesn't compress it again.

use std::io::Write;
use std::path::{Path, PathBuf};

use proc_macro::TokenStream;
use proc_macro2::Literal;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{bracketed, Ident, LitStr, Token};

enum Source {
    /// Pieces of one path under the crate's directory.
    Manifest(Vec<LitStr>),
    /// A path under the build script's output.
    OutDir(LitStr),
    /// Files under the crate's directory, with a separator between them.
    Join(LitStr, Vec<LitStr>),
}

impl Parse for Source {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        if input.peek(Ident) {
            let mode: Ident = input.parse()?;
            return match mode.to_string().as_str() {
                "out_dir" => Ok(Source::OutDir(input.parse()?)),
                "join" => {
                    let separator = input.parse()?;
                    let content;
                    bracketed!(content in input);
                    let files = content.parse_terminated(<LitStr as Parse>::parse, Token![,])?;
                    Ok(Source::Join(separator, files.into_iter().collect()))
                }
                other => Err(syn::Error::new(
                    mode.span(),
                    format!("bake!: no mode {other}"),
                )),
            };
        }
        let pieces = input.parse_terminated(<LitStr as Parse>::parse, Token![,])?;
        Ok(Source::Manifest(pieces.into_iter().collect()))
    }
}

#[proc_macro]
pub fn bake(input: TokenStream) -> TokenStream {
    let source = syn::parse_macro_input!(input as Source);
    match expand(source) {
        Ok(tokens) => tokens.into(),
        Err(message) => quote! { compile_error!(#message) }.into(),
    }
}

fn env_dir(name: &str) -> Result<PathBuf, String> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| format!("bake!: {name} isn't set"))
}

fn expand(source: Source) -> Result<proc_macro2::TokenStream, String> {
    let manifest = || env_dir("CARGO_MANIFEST_DIR");
    let (files, separator) = match &source {
        Source::Manifest(pieces) => {
            let path: String = pieces.iter().map(LitStr::value).collect();
            (vec![manifest()?.join(path)], None)
        }
        Source::OutDir(path) => {
            let out = env_dir("OUT_DIR")?;
            (vec![out.join(path.value().trim_start_matches('/'))], None)
        }
        Source::Join(separator, files) => {
            let root = manifest()?;
            (
                files.iter().map(|file| root.join(file.value())).collect(),
                Some(separator.value()),
            )
        }
    };
    let mut bytes = Vec::new();
    for (i, file) in files.iter().enumerate() {
        if i > 0 {
            bytes.extend_from_slice(separator.as_deref().unwrap_or("").as_bytes());
        }
        let read = std::fs::read(file)
            .map_err(|e| format!("bake!: can't read {}: {e}", file.display()))?;
        bytes.extend_from_slice(&read);
    }
    // Each file named to the compiler, so it compiles again when one changes.
    let tracked: Vec<String> = files
        .iter()
        .map(|file| file.display().to_string())
        .collect();
    let raw = if files.len() == 1 {
        let file = &tracked[0];
        quote! { include_bytes!(#file) }
    } else {
        let literal = Literal::byte_string(&bytes);
        quote! { #literal }
    };
    let compressible = files.iter().all(|file| {
        matches!(
            file.extension().and_then(|e| e.to_str()),
            Some("js" | "mjs" | "css" | "svg" | "wasm" | "json" | "txt" | "html")
        )
    });
    let copy = |encoding: Encoding| -> proc_macro2::TokenStream {
        match compressible.then(|| compressed(&bytes, encoding)).flatten() {
            Some(copy) => {
                let literal = Literal::byte_string(&copy);
                quote! { ::core::option::Option::Some(#literal) }
            }
            None => quote! { ::core::option::Option::None },
        }
    };
    let (brotli, gzip) = (copy(Encoding::Brotli), copy(Encoding::Gzip));
    Ok(quote! {
        {
            #( const _: &[u8] = include_bytes!(#tracked); )*
            ::static_assets::Baked { bytes: #raw, brotli: #brotli, gzip: #gzip }
        }
    })
}

#[derive(Clone, Copy)]
enum Encoding {
    Brotli,
    Gzip,
}

/// `bytes` in `encoding`, if that makes them smaller: from the cache when
/// this content was compressed before.
fn compressed(bytes: &[u8], encoding: Encoding) -> Option<Vec<u8>> {
    // The smallest brotli, except for the WebAssembly modules (most of a
    // megabyte), where it would take seconds and save little.
    let quality = match encoding {
        Encoding::Brotli if bytes.len() <= 256 * 1024 => 11,
        Encoding::Brotli => 9,
        Encoding::Gzip => 9,
    };
    let extension = match encoding {
        Encoding::Brotli => "br",
        Encoding::Gzip => "gz",
    };
    let cached = cache_dir().map(|dir| {
        dir.join(format!(
            "{:016x}-{}-q{quality}.{extension}",
            fnv1a64(bytes),
            bytes.len()
        ))
    });
    let copy = match cached.as_ref().and_then(|path| std::fs::read(path).ok()) {
        Some(copy) => copy,
        None => {
            let copy = compress(bytes, encoding, quality);
            if let Some(path) = &cached {
                keep(path, &copy);
            }
            copy
        }
    };
    (copy.len() < bytes.len()).then_some(copy)
}

fn compress(bytes: &[u8], encoding: Encoding, quality: u32) -> Vec<u8> {
    match encoding {
        Encoding::Brotli => {
            let mut out = Vec::new();
            let params = brotli::enc::BrotliEncoderParams {
                quality: quality as i32,
                ..Default::default()
            };
            brotli::BrotliCompress(&mut &bytes[..], &mut out, &params).expect("brotli in memory");
            out
        }
        Encoding::Gzip => {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(quality));
            encoder.write_all(bytes).expect("gzip in memory");
            encoder.finish().expect("gzip in memory")
        }
    }
}

/// The build's cache of compressed copies: beside its `build/` folder,
/// found from the build script's `OUT_DIR`. None for a crate without one,
/// whose files are compressed each time.
fn cache_dir() -> Option<PathBuf> {
    let out = PathBuf::from(std::env::var_os("OUT_DIR")?);
    let build = out
        .ancestors()
        .find(|dir| dir.file_name().is_some_and(|name| name == "build"))?;
    let dir = build.parent()?.join("static-assets-cache");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Writes a copy into the cache whole or not at all: another compile may be
/// reading it.
fn keep(path: &Path, copy: &[u8]) {
    let partial = path.with_extension(format!("partial-{}", std::process::id()));
    if std::fs::write(&partial, copy).is_ok() && std::fs::rename(&partial, path).is_err() {
        let _ = std::fs::remove_file(&partial);
    }
}

/// FNV-1a, as `static_assets` names files: here only to name cached copies.
fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}
