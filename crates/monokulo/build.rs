//! Builds the browser code the binary embeds into `OUT_DIR`; the built files
//! are not kept in git:
//!
//! - the POS terminal app (`apps/pos`, Solid 2 + Vite), which
//!   `crate::assets` embeds;
//! - the server-rendered pages' scripts (`apps/engine`, the same
//!   toolchain): the shared components' islands (`crates/ui/web/islands.ts`)
//!   and the engine page's live view, in chunks, with a list of the chunks
//!   for `crate::assets` (`web/chunks.rs`);
//! - key custody's browser module (the `key-custody` crate for
//!   `wasm32-unknown-unknown`), which `http::key_entry` embeds.
//!
//! A debug build gets Solid's development build (its diagnostics, unminified
//! code); a release build gets the minified production one.
//! `MONOKULO_POS_BUILD=development|production` overrides the choice, and
//! `MONOKULO_POS_BUILD=none` builds no browser code at all (needing neither
//! Node nor the WebAssembly target), for writing the API's reference: a
//! binary built so won't run.
//!
//! Needs Node and the apps' dependencies installed from their lockfile (`npm
//! ci` at the repository root). This script never installs anything itself:
//! a build that reaches the network would break offline builds and make
//! builds depend on the registry.

// Built separately for WebAssembly (`build_key_custody_wasm`,
// `build_wallet_setup_wasm`); build dependencies only so their crates are
// fetched first.
extern crate key_custody as _;
extern crate wallet_setup as _;

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rerun-if-env-changed=MONOKULO_POS_BUILD");
    println!("cargo::rustc-check-cfg=cfg(monokulo_without_browser_code)");

    let mode = match std::env::var("MONOKULO_POS_BUILD").ok().as_deref() {
        Some("development") => "development",
        Some("production") => "production",
        Some("none") => return without_browser_code(),
        Some(other) => panic!(
            "MONOKULO_POS_BUILD must be \"development\", \"production\" or \"none\", got {other:?}"
        ),
        None if std::env::var("PROFILE").as_deref() == Ok("release") => "production",
        None => "development",
    };

    let workspace = manifest.join("../..");
    for input in [
        "crates/ui/web",
        "crates/ui/solid",
        "crates/ui/package.json",
        "package.json",
        "package-lock.json",
    ] {
        println!("cargo:rerun-if-changed={}", workspace.join(input).display());
    }
    let vite = workspace.join("node_modules/vite/bin/vite.js");
    let tsc = workspace.join("node_modules/typescript/bin/tsc");
    if !vite.exists() || !tsc.exists() {
        panic!(
            "\n\nThe POS app's dependencies are not installed. Install them from the lockfile:\n\n    \
             cd {} && npm ci\n\n(Node 24 or later is required to build monokulo.)\n",
            workspace.display()
        );
    }
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    let pos = build_app(
        &workspace.join("apps/pos"),
        &out_dir.join("pos"),
        &vite,
        &tsc,
        mode,
    );
    for file in ["pos-app.js", "pos-app.css"] {
        assert!(
            pos.join(file).exists(),
            "the POS build did not produce {file}"
        );
    }

    // The server-rendered pages' scripts: the entries are linked by name
    // (`crate::assets` puts its digest in their URL), every other chunk is
    // served at the name Vite gave it, its hash in it.
    let web = build_app(
        &workspace.join("apps/engine"),
        &out_dir.join("web"),
        &vite,
        &tsc,
        mode,
    );
    let entries = ["ui-islands.js", "engine-view.js"];
    for file in entries.iter().chain(&["engine-view.css"]) {
        assert!(
            web.join(file).exists(),
            "the pages' build did not produce {file}"
        );
    }
    let mut chunks: Vec<String> = std::fs::read_dir(&web)
        .expect("the pages' build output")
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".js") && !entries.contains(&name.as_str()))
        .collect();
    chunks.sort();
    let list: String = chunks
        .iter()
        .map(|name| {
            format!(
                "    static_assets::asset!(named {name:?}, \"/web/{name}\", static_assets::JS),\n"
            )
        })
        .collect();
    std::fs::write(web.join("chunks.rs"), format!("&[\n{list}]\n")).expect("web/chunks.rs");
    write_preloads(&web);

    build_key_custody_wasm(&manifest);
    build_wallet_setup_wasm(&manifest);
    release_identity();
}

/// `MONOKULO_POS_BUILD=none`: no browser code at all, so neither Node nor
/// the WebAssembly target is needed. Every file the binary embeds is an
/// empty stand-in, and `monokulo_without_browser_code` is set, so the
/// binary refuses to serve (`main`). For what never shows a page: `cargo
/// xtask api openapi` writing the API's reference.
fn without_browser_code() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    for dir in ["pos", "web"] {
        std::fs::create_dir_all(out.join(dir)).expect("OUT_DIR");
    }
    for file in [
        "pos/pos-app.js",
        "pos/pos-app.css",
        "web/ui-islands.js",
        "web/engine-view.js",
        "web/engine-view.css",
        "key_custody.wasm",
        "wallet_setup.wasm",
    ] {
        std::fs::write(out.join(file), "").expect("a stand-in in OUT_DIR");
    }
    for list in ["web/chunks.rs", "web/preloads.rs"] {
        std::fs::write(out.join(list), "&[]\n").expect("a stand-in in OUT_DIR");
    }
    println!("cargo:rustc-cfg=monokulo_without_browser_code");
    release_identity();
}

/// Type-checks one of `apps/` and builds it with Vite into `out`, which it
/// returns.
fn build_app(app: &Path, out: &Path, vite: &Path, tsc: &Path, mode: &str) -> PathBuf {
    for input in [
        "src",
        "package.json",
        "vite.config.ts",
        "tsconfig.json",
        "chunks.config.ts",
    ] {
        // Only what the app has: Cargo takes a path that doesn't exist for
        // one that changed, and would build again every time.
        let path = app.join(input);
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    run(app, tsc, &["--noEmit"], mode);
    run(
        app,
        vite,
        &[
            "build",
            "--mode",
            mode,
            "--outDir",
            out.to_str().unwrap(),
            "--emptyOutDir",
        ],
        mode,
    );
    out.to_path_buf()
}

/// `web/preloads.rs`, from Vite's manifest: the chunks each entry imports
/// (by its file, `"engine-view.js"`) and each island's behaviour needs (by
/// the component's name, `"Select"`), its own chunk first. A page asks for
/// them with the script, rather than once the script has run.
fn write_preloads(web: &Path) {
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(web.join(".vite/manifest.json")).expect("the pages' build's manifest"),
    )
    .expect("the manifest is JSON");
    let manifest = manifest.as_object().expect("the manifest is an object");
    let file = |key: &str| manifest[key]["file"].as_str().unwrap().to_string();
    // Every chunk a manifest entry imports, and what those import.
    fn imports(
        manifest: &serde_json::Map<String, serde_json::Value>,
        key: &str,
        into: &mut Vec<String>,
    ) {
        for import in manifest[key]["imports"].as_array().into_iter().flatten() {
            let import = import.as_str().unwrap();
            let file = manifest[import]["file"].as_str().unwrap().to_string();
            if !into.contains(&file) {
                into.push(file);
                imports(manifest, import, into);
            }
        }
    }
    let mut rows = String::new();
    let mut row = |name: String, key: &str, mut files: Vec<String>| {
        imports(manifest, key, &mut files);
        rows.push_str(&format!("    ({name:?}, &{files:?}),\n"));
    };
    for (key, chunk) in manifest {
        if chunk["isEntry"].as_bool() == Some(true) {
            row(file(key), key, Vec::new());
        }
    }
    // Each island's behaviour, as the islands' script imports it: its own
    // chunk, which another entry may import too (the engine page's Select).
    let islands = manifest
        .iter()
        .find(|(_, chunk)| chunk["file"] == "ui-islands.js")
        .map(|(_, chunk)| chunk)
        .expect("the islands' script in the manifest");
    for key in islands["dynamicImports"].as_array().into_iter().flatten() {
        let key = key.as_str().unwrap();
        let name = manifest[key]["name"].as_str().unwrap().to_string();
        row(name, key, vec![file(key)]);
    }
    std::fs::write(web.join("preloads.rs"), format!("&[\n{rows}]\n")).expect("web/preloads.rs");
}

/// A browser module built from one of the workspace's crates.
struct BrowserModule {
    /// What the build failure message calls it.
    what: &'static str,
    package: &'static str,
    /// The crate's own sources and anything else that changes its build.
    inputs: &'static [&'static str],
    /// The `.wasm` the build makes, and the name it's copied to in `OUT_DIR`.
    artifact: &'static str,
}

/// Builds the `key-custody` crate's browser module (`--features wasm`,
/// without its backends) into `OUT_DIR/key_custody.wasm`.
fn build_key_custody_wasm(manifest: &Path) {
    build_browser_module(
        manifest,
        &BrowserModule {
            what: "key custody's browser module",
            package: "key-custody",
            inputs: &[
                "crates/key-custody/src",
                "crates/key-custody/Cargo.toml",
                "crates/snp-attest/src",
                "crates/snp-attest/Cargo.toml",
            ],
            artifact: "key_custody.wasm",
        },
    );
}

/// Builds the `wallet-setup` crate's browser module (`--features wasm`) into
/// `OUT_DIR/wallet_setup.wasm`: the "Create a new wallet" page's recovery
/// phrase, keys and QR codes.
fn build_wallet_setup_wasm(manifest: &Path) {
    build_browser_module(
        manifest,
        &BrowserModule {
            what: "the new wallet page's browser module",
            package: "wallet-setup",
            inputs: &["crates/wallet-setup/src", "crates/wallet-setup/Cargo.toml"],
            artifact: "wallet_setup.wasm",
        },
    );
}

/// Builds `module` for WebAssembly into `OUT_DIR`, with the same lockfile,
/// offline: its dependencies are this crate's build dependencies too, so
/// they're already fetched. Always optimised for size, whatever this build's
/// profile: the pages that use it download it.
fn build_browser_module(manifest: &Path, module: &BrowserModule) {
    let workspace = manifest.join("../..");
    for input in module
        .inputs
        .iter()
        .chain(&["Cargo.lock", ".cargo/config.toml"])
    {
        println!("cargo:rerun-if-changed={}", workspace.join(input).display());
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // A target directory of its own beside this build's, not in `OUT_DIR`:
    // `OUT_DIR` differs with every profile and feature set, so each would
    // build the module again, and CI's Rust cache drops a workspace crate's
    // `OUT_DIR` (it keeps a nested target directory's dependencies).
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| workspace.join("target"), PathBuf::from)
        .join(format!("{}-wasm", module.package));
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let mut command = Command::new(cargo);
    command
        // From the workspace, so its `.cargo/config.toml` applies (the
        // WebAssembly target's randomness comes from the page).
        .current_dir(&workspace)
        .args([
            "build",
            "--offline",
            "--locked",
            "--release",
            "--package",
            module.package,
            "--lib",
            "--no-default-features",
            "--features",
            "wasm",
            "--target",
            "wasm32-unknown-unknown",
            "--target-dir",
        ])
        .arg(&target_dir)
        .env("CARGO_PROFILE_RELEASE_OPT_LEVEL", "s")
        .env("CARGO_PROFILE_RELEASE_LTO", "true")
        .env("CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "1")
        .env("CARGO_PROFILE_RELEASE_PANIC", "abort")
        .env("CARGO_PROFILE_RELEASE_STRIP", "true");
    // What this build was given for its own target (coverage
    // instrumentation, a wrapper) is not for WebAssembly.
    for var in [
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_BUILD_RUSTFLAGS",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_TARGET",
        "CARGO_MAKEFLAGS",
        "LLVM_PROFILE_FILE",
    ] {
        command.env_remove(var);
    }
    let what = module.what;
    let status = command
        .status()
        .unwrap_or_else(|e| panic!("could not run cargo to build {what}: {e}"));
    assert!(
        status.success(),
        "\n\nBuilding {what} failed. It needs the WebAssembly target \
         (rust-toolchain.toml installs it; otherwise `rustup target add wasm32-unknown-unknown`).\n"
    );
    let built = target_dir
        .join("wasm32-unknown-unknown/release")
        .join(module.artifact);
    std::fs::copy(&built, out.join(module.artifact))
        .unwrap_or_else(|e| panic!("copying {}: {e}", built.display()));
}

/// Which release this build is, for the key-custody-cli download links: CI
/// sets `MONOKULO_RELEASE_TAG` on a version tag's build, and
/// `MONOKULO_GIT_COMMIT` on every build it makes.
fn release_identity() {
    for var in ["MONOKULO_RELEASE_TAG", "MONOKULO_GIT_COMMIT"] {
        println!("cargo:rerun-if-env-changed={var}");
    }
}

fn run(ui: &Path, script: &Path, args: &[&str], mode: &str) {
    let status = Command::new("node")
        .arg(script)
        .args(args)
        .current_dir(ui)
        .env("NODE_ENV", mode)
        .status()
        .unwrap_or_else(|e| {
            panic!("could not run node (Node 24 or later is required to build monokulo): {e}")
        });
    assert!(
        status.success(),
        "{} {} failed",
        script.display(),
        args.join(" ")
    );
}
