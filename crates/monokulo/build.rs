//! Builds the POS terminal app (`pos-ui/`, Solid 2 + Vite) into `OUT_DIR`,
//! where `http::pay` embeds it, and key custody's browser module (the
//! `key-custody` crate for `wasm32-unknown-unknown`), which `http::key_entry`
//! embeds; the built files are not kept in git.
//!
//! A debug build gets Solid's development build (its diagnostics, unminified
//! code); a release build gets the minified production one.
//! `MONOKULO_POS_BUILD=development|production` overrides the choice.
//!
//! Needs Node and the app's dependencies installed from its lockfile (`npm ci`
//! in `crates/monokulo/pos-ui`). This script never installs anything itself:
//! a build that reaches the network would break offline builds and make
//! builds depend on the registry.

// Built separately for WebAssembly (`build_key_custody_wasm`); a build
// dependency only so its crates are fetched first.
extern crate key_custody as _;

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let ui = manifest.join("pos-ui");
    for input in [
        "src",
        "package.json",
        "package-lock.json",
        "vite.config.ts",
        "tsconfig.json",
    ] {
        println!("cargo:rerun-if-changed={}", ui.join(input).display());
    }
    println!("cargo:rerun-if-env-changed=MONOKULO_POS_BUILD");

    let mode = match std::env::var("MONOKULO_POS_BUILD").ok().as_deref() {
        Some("development") => "development",
        Some("production") => "production",
        Some(other) => {
            panic!("MONOKULO_POS_BUILD must be \"development\" or \"production\", got {other:?}")
        }
        None if std::env::var("PROFILE").as_deref() == Ok("release") => "production",
        None => "development",
    };

    let vite = ui.join("node_modules/vite/bin/vite.js");
    let tsc = ui.join("node_modules/typescript/bin/tsc");
    if !vite.exists() || !tsc.exists() {
        panic!(
            "\n\nThe POS app's dependencies are not installed. Install them from the lockfile:\n\n    \
             cd {} && npm ci\n\n(Node 24 or later is required to build monokulo.)\n",
            ui.display()
        );
    }

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("pos-ui");
    run(&ui, &tsc, &["--noEmit"], mode);
    run(
        &ui,
        &vite,
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
    for file in ["pos-app.js", "pos-app.css"] {
        assert!(
            out.join(file).exists(),
            "the POS build did not produce {file}"
        );
    }

    build_key_custody_wasm(&manifest);
    release_identity();
}

/// Builds the `key-custody` crate's browser module (`--features wasm`,
/// without its backends) into `OUT_DIR/key_custody.wasm`, with the same
/// lockfile, offline: its dependencies are this crate's build dependencies
/// too, so they're already fetched. Always optimised for size, whatever this
/// build's profile: it is downloaded by every key entry form.
fn build_key_custody_wasm(manifest: &Path) {
    let workspace = manifest.join("../..");
    for input in [
        "crates/key-custody/src",
        "crates/key-custody/Cargo.toml",
        "crates/snp-attest/src",
        "crates/snp-attest/Cargo.toml",
        "Cargo.lock",
        ".cargo/config.toml",
    ] {
        println!("cargo:rerun-if-changed={}", workspace.join(input).display());
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let target_dir = out.join("key-custody-wasm");
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
            "key-custody",
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
    let status = command.status().unwrap_or_else(|e| {
        panic!("could not run cargo to build key custody's browser module: {e}")
    });
    assert!(
        status.success(),
        "\n\nBuilding key custody's browser module failed. It needs the WebAssembly target \
         (rust-toolchain.toml installs it; otherwise `rustup target add wasm32-unknown-unknown`).\n"
    );
    let built = target_dir.join("wasm32-unknown-unknown/release/key_custody.wasm");
    std::fs::copy(&built, out.join("key_custody.wasm"))
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
