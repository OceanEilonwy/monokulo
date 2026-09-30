//! Builds the POS terminal app (`pos-ui/`, Solid 2 + Vite) into `OUT_DIR`,
//! where `http::pay` embeds it; the built files are not kept in git.
//!
//! A debug build gets Solid's development build (its diagnostics, unminified
//! code); a release build gets the minified production one.
//! `MONOKULO_POS_BUILD=development|production` overrides the choice.
//!
//! Needs Node and the app's dependencies installed from its lockfile (`npm ci`
//! in `crates/monokulo/pos-ui`). This script never installs anything itself:
//! a build that reaches the network would break offline builds and make
//! builds depend on the registry.

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
