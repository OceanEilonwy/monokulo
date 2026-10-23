//! `cargo xtask api openapi --out FILE`: monokulo's HTTP API described, from
//! the code and from real exchanges, and never committed. Monokulo's
//! `api::runtime::reference` test makes one request of each kind against a
//! real engine, checks the answers against the schemas, and, with
//! `MONOKULO_API_OUT` set, writes `examples.json`, `openapi.json` and
//! `reference.json` there. The docs build (`cargo xtask pages api`) and a
//! plugin wanting the OpenAPI document at a pinned commit both run it.

use crate::support::{at, Exit};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

pub(crate) const HELP: &str = "\
        api openapi --out FILE\n\
                      Write the HTTP API's OpenAPI document, with examples recorded from monokulo's\n\
                      tests, to FILE (runs monokulo's api::runtime::reference test)";

/// The test that records the exchanges and writes the reference.
const TEST: &str = "api::runtime::reference::tests::the_reference_is_written_from_real_exchanges";

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

/// Runs the recording test with its output in `dir`: `examples.json`,
/// `openapi.json` and `reference.json`.
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
        .args(["test", "-p", "monokulo", "--lib", "--"])
        .args(["--exact", TEST])
        .env("MONOKULO_API_OUT", &dir)
        .current_dir(root)
        .status()?;
    if !status.success() {
        eprintln!("{TEST} failed, so the API reference wasn't written");
        return Ok(Exit::of(status));
    }
    let reference = dir.join("reference.json");
    if !reference.is_file() {
        return Err(at(
            &reference,
            io::Error::other("the test passed but wrote nothing (was it filtered out?)"),
        ));
    }
    Ok(Exit::SUCCESS)
}
