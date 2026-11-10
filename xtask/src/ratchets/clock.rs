//! Time is told by a `clock::Clock` passed in, so a test can say what time
//! it is (crates/clock). Clippy refuses the system's clock outright
//! (`clippy.toml`); these two checks keep the rest of the rule:
//!
//! - `Clock::System` is chosen only where a process starts (a `main.rs`, a
//!   binary, xtask, the e2e harness) or where a reason below says why, so
//!   no library decides the time for its caller;
//! - a moment is a `clock::UnixTime`, never a bare `i64`: no parameter or
//!   field named `now` is an `i64`, so nobody does arithmetic on one.

use super::{pruned, relative};
use crate::support::{at, files_under};
use std::{fs, io, path::Path};
use syn::visit::{self, Visit};

/// Library code that may choose `Clock::System`, by file, and why.
pub(super) const SYSTEM_ALLOWED: [(&str, &str); 2] = [
    (
        "crates/engine/src/run.rs",
        "Engine::start, where an engine process starts, for its binary and for monokulo's embedded engine (tests start theirs through engine-test-support with their own clock)",
    ),
    (
        "crates/abuse/src/lib.rs",
        "AbuseProtection's Default, which every caller replaces with its own clock (`with_clock`)",
    ),
];

/// Whether a file is where a process starts, so choosing the system's
/// clock there is the point.
fn entry_point(file: &str) -> bool {
    file.ends_with("/main.rs")
        || file.contains("/src/bin/")
        || file.contains("/examples/")
        || file.starts_with("xtask/")
        || file.starts_with("crates/e2e-harness/")
        || file.starts_with("crates/clock/")
}

/// Whether a file is only ever test code.
fn test_file(file: &str) -> bool {
    file.split('/')
        .any(|part| part == "tests" || part == "benches")
        || file.ends_with("/tests.rs")
        || file.ends_with("_tests.rs")
}

/// Whether an item's attributes make it test code: `#[test]`,
/// `#[tokio::test]`, or a `cfg` that names `test`.
fn test_attrs(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        let path = attr.path();
        if path.segments.last().is_some_and(|s| s.ident == "test") {
            return true;
        }
        path.is_ident("cfg")
            && attr.meta.require_list().is_ok_and(|list| {
                list.tokens
                    .to_string()
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .any(|word| word == "test")
            })
    })
}

/// What the two checks find in one file.
#[derive(Default)]
struct Found {
    /// `Clock::System` outside test code.
    system: usize,
    /// Parameters and fields named `now` typed `i64`, by the function or
    /// type they're in.
    bare_now: Vec<String>,
}

struct Visitor {
    found: Found,
    in_test: usize,
    /// The functions and types being walked, innermost last.
    within: Vec<String>,
}

fn named_now(ident: &syn::Ident) -> bool {
    matches!(ident.to_string().as_str(), "now" | "now_unix" | "now_secs")
}

fn is_i64(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Path(path) if path.qself.is_none() && path.path.is_ident("i64"))
}

impl Visitor {
    fn test_scope(
        &mut self,
        attrs: &[syn::Attribute],
        name: Option<&syn::Ident>,
        inner: impl FnOnce(&mut Self),
    ) {
        let test = test_attrs(attrs);
        self.in_test += usize::from(test);
        if let Some(name) = name {
            self.within.push(name.to_string());
        }
        inner(self);
        if name.is_some() {
            self.within.pop();
        }
        self.in_test -= usize::from(test);
    }

    fn here(&self) -> String {
        self.within.last().cloned().unwrap_or_default()
    }
}

impl Visit<'_> for Visitor {
    fn visit_item_mod(&mut self, item: &syn::ItemMod) {
        self.test_scope(&item.attrs, None, |v| visit::visit_item_mod(v, item));
    }
    fn visit_item_fn(&mut self, item: &syn::ItemFn) {
        self.test_scope(&item.attrs, Some(&item.sig.ident), |v| {
            visit::visit_item_fn(v, item);
        });
    }
    fn visit_item_impl(&mut self, item: &syn::ItemImpl) {
        self.test_scope(&item.attrs, None, |v| visit::visit_item_impl(v, item));
    }
    fn visit_impl_item_fn(&mut self, item: &syn::ImplItemFn) {
        self.test_scope(&item.attrs, Some(&item.sig.ident), |v| {
            visit::visit_impl_item_fn(v, item);
        });
    }
    fn visit_item_struct(&mut self, item: &syn::ItemStruct) {
        self.test_scope(&item.attrs, Some(&item.ident), |v| {
            visit::visit_item_struct(v, item);
        });
    }
    fn visit_path(&mut self, path: &syn::Path) {
        let names: Vec<_> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        if self.in_test == 0 && names.ends_with(&["Clock".to_owned(), "System".to_owned()]) {
            self.found.system += 1;
        }
        visit::visit_path(self, path);
    }
    fn visit_pat_type(&mut self, typed: &syn::PatType) {
        if let syn::Pat::Ident(pat) = typed.pat.as_ref() {
            if named_now(&pat.ident) && is_i64(&typed.ty) {
                self.found.bare_now.push(self.here());
            }
        }
        visit::visit_pat_type(self, typed);
    }
    fn visit_field(&mut self, field: &syn::Field) {
        if field.ident.as_ref().is_some_and(named_now) && is_i64(&field.ty) {
            self.found.bare_now.push(self.here());
        }
        visit::visit_field(self, field);
    }
}

fn scan(file: &str, text: &str) -> Found {
    let Ok(parsed) = syn::parse_file(text) else {
        return Found::default();
    };
    let mut visitor = Visitor {
        found: Found::default(),
        in_test: usize::from(test_file(file)),
        within: Vec::new(),
    };
    visitor.visit_file(&parsed);
    visitor.found
}

/// Each problem the two checks find under `root`'s crates and xtask.
pub(super) fn problems(root: &Path) -> io::Result<Vec<String>> {
    let mut problems = Vec::new();
    for dir in ["crates", "xtask"] {
        for path in files_under(&root.join(dir), pruned)? {
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let file = relative(root, &path);
            let text = fs::read_to_string(&path).map_err(|e| at(&path, e))?;
            let found = scan(&file, &text);
            let allowed = entry_point(&file) || SYSTEM_ALLOWED.iter().any(|(f, _)| *f == file);
            if found.system > 0 && !allowed {
                problems.push(format!(
                    "{file}: chooses Clock::System outside a process's start (take a clock::Clock from the caller, or name why in ratchets::clock::SYSTEM_ALLOWED)"
                ));
            }
            for within in found.bare_now {
                problems.push(format!(
                    "{file} ({within}): a moment named `now` is a bare i64 (make it a clock::UnixTime)"
                ));
            }
        }
    }
    problems.sort();
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_library_chooses_the_system_clock_and_no_moment_is_a_bare_i64() {
        let problems = problems(&crate::support::root()).unwrap();
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    #[test]
    fn every_reasoned_exception_is_still_needed() {
        let root = crate::support::root();
        for (file, _) in SYSTEM_ALLOWED {
            let text = fs::read_to_string(root.join(file)).unwrap();
            assert!(
                scan(file, &text).system > 0,
                "{file} no longer chooses Clock::System"
            );
        }
    }

    /// What each check flags and what it lets through.
    #[test]
    fn only_library_code_choosing_the_system_clock_and_bare_i64_moments_are_flagged() {
        let text = r#"
            fn decides() -> clock::Clock { clock::Clock::System }
            fn told(clock: &clock::Clock) -> clock::UnixTime { clock.now() }
            fn bare(now: i64) {}
            fn typed(now: clock::UnixTime, at: i64) {}
            struct Row { now: i64, created_at: i64 }
            #[cfg(test)] mod tests { fn pinned() { let _ = clock::Clock::System; } fn helper(now: i64) {} }
            #[cfg(any(test, feature = "test-support"))] fn support() -> clock::Clock { clock::Clock::System }
            #[tokio::test] async fn a_test() { let _ = clock::Clock::System.now(); }
        "#;
        let found = scan("crates/x/src/lib.rs", text);
        assert_eq!(found.system, 1, "only `decides`");
        assert_eq!(found.bare_now, ["bare", "Row", "helper"]);
        assert_eq!(
            scan("crates/x/tests/it.rs", text).system,
            0,
            "a tests directory is test code"
        );
        assert!(entry_point("crates/x/src/main.rs") && entry_point("crates/x/src/bin/tool.rs"));
        assert!(!entry_point("crates/x/src/lib.rs"));
    }
}
