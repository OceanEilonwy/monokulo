//! `cargo xtask ui`: writes what the `ui` crate's components translate to.
//!
//! The components are written once, in crates/ui. From them this writes:
//!
//! - packages/ui/src/solid: the Solid components the POS imports;
//! - apps/stylebook/examples/generated: the stylebook's section for each
//!   component, with its examples rendered by the Rust components;
//! - packages/ui/tests/solid/generated/fixtures.json: the same examples as props and
//!   HTML, which the browser suite renders through the Solid components and
//!   compares.
//!
//! Each directory is owned whole: a file no component writes any more is
//! removed. `--check` writes nothing and fails if anything is out of date,
//! as CI runs it.

use crate::support::Exit;
use std::{collections::BTreeMap, fs, io, path::Path};

pub(crate) const HELP: &str =
    "        ui [--check]  Write the Solid components, stylebook sections and fixtures from crates/ui";

const SOLID: &str = "packages/ui/src/solid";
const BOOK: &str = "apps/stylebook/examples/generated";
const FIXTURES: &str = "packages/ui/tests/solid/generated";

/// Every generated file, by directory and name.
fn outputs() -> BTreeMap<&'static str, BTreeMap<String, String>> {
    let catalogue = ui::components::catalogue();
    let infos: Vec<_> = catalogue.iter().map(|s| s.info).collect();
    BTreeMap::from([
        (SOLID, ui::solid::files(&infos)),
        (
            BOOK,
            BTreeMap::from([
                ("nav.html".to_string(), ui::book::nav(&catalogue)),
                (
                    "components.html".to_string(),
                    ui::book::sections(&catalogue),
                ),
            ]),
        ),
        (
            FIXTURES,
            BTreeMap::from([("fixtures.json".to_string(), ui::book::fixtures(&catalogue))]),
        ),
    ])
}

pub(crate) fn ui(root: &Path, args: &[&str]) -> io::Result<Exit> {
    let check = match args {
        [] => false,
        ["--check"] => true,
        _ => return Err(io::Error::other("usage: cargo xtask ui [--check]")),
    };
    let mut stale = Vec::new();
    for (dir, files) in outputs() {
        let dir = root.join(dir);
        let existing: Vec<String> = match fs::read_dir(&dir) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .filter(|e| e.path().is_file())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        for name in existing.iter().filter(|n| !files.contains_key(*n)) {
            stale.push(dir.join(name));
            if !check {
                fs::remove_file(dir.join(name))?;
            }
        }
        if !check {
            fs::create_dir_all(&dir)?;
        }
        for (name, content) in files {
            let path = dir.join(&name);
            if fs::read_to_string(&path).ok().as_deref() == Some(content.as_str()) {
                continue;
            }
            stale.push(path.clone());
            if !check {
                fs::write(&path, content)?;
            }
        }
    }
    if stale.is_empty() {
        println!("The generated UI files are up to date.");
        return Ok(Exit::SUCCESS);
    }
    for path in &stale {
        let path = path.strip_prefix(root).unwrap_or(path).display();
        println!("{} {path}", if check { "out of date:" } else { "wrote" });
    }
    if check {
        println!("Run `cargo xtask ui` and commit what it writes.");
        return Ok(Exit::FAILURE);
    }
    Ok(Exit::SUCCESS)
}
