//! `cargo xtask api openapi --out FILE`: monokulo's HTTP API described, from
//! the code and from real exchanges, and never committed. Monokulo's
//! `api_reference` example makes one request of each kind against a test
//! engine, checks each answer against the schemas, and writes
//! `examples.json`, `openapi.json` and `reference.json`. The docs build
//! (`cargo xtask pages api`) and a plugin or app generating its client
//! both run it.
//!
//! It builds monokulo's library and that example only: no tests, and, with
//! `MONOKULO_POS_BUILD=none`, no browser code, so it needs neither Node nor
//! the WebAssembly target installed by hand (rustup installs the toolchain
//! `rust-toolchain.toml` names by itself).

use crate::support::{at, Exit};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

pub(crate) const HELP: &str = "\
        api openapi --out FILE\n\
                      Write the HTTP API's OpenAPI document, with examples recorded against a test\n\
                      engine, to FILE (builds monokulo's api_reference example; no tests, no Node)";

/// `cargo xtask api ...`.
pub(crate) fn api(root: &Path, args: &[&str]) -> io::Result<Exit> {
    let out = match args {
        ["openapi", "--out", file] => PathBuf::from(file),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "api needs: openapi --out FILE",
            ))
        }
    };
    let dir = root.join("target/api");
    let exit = generate(root, &dir)?;
    if !exit.succeeded() {
        return Ok(exit);
    }
    let from = dir.join("openapi.json");
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|e| at(parent, e))?;
    }
    fs::copy(&from, &out).map_err(|e| at(&from, e))?;
    eprintln!("OpenAPI document in {}", out.display());
    Ok(Exit::SUCCESS)
}

/// Runs monokulo's `api_reference` example with its output in `dir`:
/// `examples.json`, `openapi.json` and `reference.json`.
pub(crate) fn generate(root: &Path, dir: &Path) -> io::Result<Exit> {
    let dir = if dir.is_absolute() {
        dir.to_owned()
    } else {
        std::env::current_dir()?.join(dir)
    };
    // Nothing left from an earlier run passes for this one's output.
    for name in ["examples.json", "openapi.json", "reference.json"] {
        let _ = fs::remove_file(dir.join(name));
    }
    let status = Command::new("cargo")
        .args([
            "run",
            "--quiet",
            "-p",
            "monokulo",
            "--example",
            "api_reference",
            "--",
        ])
        .arg(&dir)
        .env("MONOKULO_POS_BUILD", "none")
        .current_dir(root)
        .status()?;
    if !status.success() {
        eprintln!("monokulo's api_reference example failed, so the API reference wasn't written");
        return Ok(Exit::of(status));
    }
    let reference = dir.join("reference.json");
    if !reference.is_file() {
        return Err(at(
            &reference,
            io::Error::other("the example finished but wrote nothing"),
        ));
    }
    Ok(Exit::SUCCESS)
}
