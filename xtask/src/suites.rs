//! `cargo xtask test`: every local test suite, the way that finishes
//! soonest. Everything is built first, before the clock starts. Then the
//! Rust tests run, then the browser tests (Chromium), each with the
//! machine to itself; the crates/ui component tests and the node tests run
//! alongside them, as they are small. Running all four side by side made
//! the browser tests nearly twice as slow through contention. Nothing is
//! instrumented or photographed: that is `cargo xtask coverage`.

use crate::support::{at, root, Exit};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Instant,
};

pub(crate) const HELP: &str = "\
        test          Build everything, then run the Rust tests, then the browser tests (Chromium),\n\
                      with the crates/ui component and node tests alongside; prints each one's wall time";

/// One suite: what it is called, and the command that runs it from the
/// repository root.
struct Runner {
    name: &'static str,
    program: &'static str,
    args: &'static [&'static str],
}

/// What the runners need built, in order. The browser binaries with the
/// command the Playwright run's global setup and fixture specs repeat
/// (real-stack.js BUILD_ARGS), so theirs find nothing to do.
const BUILD: [Runner; 3] = [
    Runner {
        name: "Rust tests",
        program: "cargo",
        args: &[
            "nextest",
            "run",
            "--workspace",
            "--locked",
            "--features",
            "monokulo/snp",
            "--no-run",
        ],
    },
    Runner {
        name: "browser binaries",
        program: "node",
        args: &["-e", "require('./e2e/browser/real-stack').buildBinaries()"],
    },
    Runner {
        name: "crates/ui",
        program: "npm",
        args: &["run", "build:ui"],
    },
];

/// The Rust tests, with nextest's default profile.
const RUST: Runner = Runner {
    name: "Rust",
    program: "cargo",
    args: &[
        "nextest",
        "run",
        "--workspace",
        "--locked",
        "--features",
        "monokulo/snp",
    ],
};

/// The browser tests' Chromium projects (coverage-browser.config.js).
const E2E: Runner = Runner {
    name: "e2e (Chromium)",
    program: "e2e/browser/node_modules/.bin/playwright",
    args: &[
        "test",
        "-c",
        "e2e/browser/coverage-browser.config.js",
        "--project",
        "fixture",
        "--project",
        "real-binaries",
    ],
};

/// The suites that run alongside, each writing to a log of its own.
const ALONGSIDE: [Runner; 3] = [
    Runner {
        name: "crates/ui components",
        program: "npm",
        args: &["run", "test:ui"],
    },
    Runner {
        name: "node: crates/ui tools",
        program: "npm",
        args: &["run", "test:ui:unit"],
    },
    Runner {
        name: "node: apps/engine",
        program: "npm",
        args: &["run", "test:engine"],
    },
];

/// What `cargo xtask coverage` sets to instrument and photograph a run.
const COVERAGE_ONLY: [&str; 4] = [
    "COVERAGE_INSTRUMENT",
    "COVERAGE_SCREENSHOTS",
    "COVERAGE_RAW_DIR",
    "COVERAGE_ASSETS_DIR",
];

impl Runner {
    fn command(&self, root: &Path) -> Command {
        let program = if self.program.contains('/') {
            root.join(self.program)
        } else {
            PathBuf::from(self.program)
        };
        let mut command = Command::new(program);
        command.args(self.args).current_dir(root);
        // xtask's own binary is the one running this, which Windows
        // won't let a build replace.
        if cfg!(windows) && self.program == "cargo" {
            command.args(["--exclude", "xtask"]);
        }
        for key in COVERAGE_ONLY {
            command.env_remove(key);
        }
        command
    }
}

/// How one suite went, and how long it took from when it started.
struct Outcome {
    name: &'static str,
    passed: bool,
    seconds: f64,
    log: Option<PathBuf>,
}

fn timed(runner: &Runner, command: &mut Command, log: Option<PathBuf>) -> Outcome {
    eprintln!("+ {} {}", runner.program, runner.args.join(" "));
    let start = Instant::now();
    let passed = command.status().is_ok_and(|s| s.success());
    Outcome {
        name: runner.name,
        passed,
        seconds: start.elapsed().as_secs_f64(),
        log,
    }
}

/// What is missing for the runners to start, if anything.
fn missing(root: &Path) -> Option<&'static str> {
    [
        (
            "node_modules/vite/bin/vite.js",
            "npm ci at the repository root",
        ),
        (
            "node_modules/@playwright/test/package.json",
            "npm ci at the repository root",
        ),
        (E2E.program, "npm ci in e2e/browser"),
    ]
    .into_iter()
    .find(|(path, _)| !root.join(path).exists())
    .map(|(_, fix)| fix)
}

/// The table `cargo xtask test` ends with.
fn table(outcomes: &[Outcome], wall: f64) -> String {
    let mut out = String::from("\nRunner                  Result  Wall\n");
    for o in outcomes {
        out.push_str(&format!(
            "{:<24}{:<8}{:>5.1} s\n",
            o.name,
            if o.passed { "passed" } else { "FAILED" },
            o.seconds
        ));
    }
    out.push_str(&format!("{:<32}{wall:>5.1} s\n", "Total"));
    for o in outcomes.iter().filter(|o| !o.passed) {
        if let Some(log) = &o.log {
            out.push_str(&format!("{}: see {}\n", o.name, log.display()));
        }
    }
    out
}

/// `cargo xtask test`.
pub(crate) fn test(args: &[&str]) -> io::Result<Exit> {
    if !args.is_empty() {
        return Err(io::Error::other("usage: cargo xtask test"));
    }
    let root = root();
    if let Some(fix) = missing(&root) {
        eprintln!("xtask test: run {fix} first");
        return Ok(Exit::FAILURE);
    }
    for runner in &BUILD {
        eprintln!("Building: {}", runner.name);
        if !timed(runner, &mut runner.command(&root), None).passed {
            eprintln!("xtask test: building {} failed", runner.name);
            return Ok(Exit::FAILURE);
        }
    }
    let logs = root.join("target/xtask-test");
    fs::create_dir_all(&logs).map_err(|e| at(&logs, e))?;
    let start = Instant::now();
    let alongside: Vec<_> = ALONGSIDE
        .iter()
        .map(|runner| {
            let path = logs.join(format!("{}.log", runner.name.replace([' ', ':', '/'], "-")));
            let log = fs::File::create(&path).map_err(|e| at(&path, e))?;
            let mut command = runner.command(&root);
            command
                .stdin(Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log);
            Ok(thread::spawn(move || {
                timed(runner, &mut command, Some(path))
            }))
        })
        .collect::<io::Result<_>>()?;
    let mut outcomes = vec![
        timed(&RUST, &mut RUST.command(&root), None),
        timed(&E2E, &mut E2E.command(&root), None),
    ];
    for handle in alongside {
        outcomes.push(handle.join().expect("a suite's thread"));
    }
    print!("{}", table(&outcomes, start.elapsed().as_secs_f64()));
    Ok(Exit::passed(outcomes.iter().all(|o| o.passed)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every script and config the runners name is still there.
    #[test]
    fn every_runner_names_a_script_and_config_that_exist() {
        let root = root();
        let package: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(root.join("package.json")).unwrap()).unwrap();
        for runner in BUILD.iter().chain(&ALONGSIDE) {
            if let ["run", script] = runner.args {
                assert!(
                    package["scripts"][script].is_string(),
                    "{}: package.json has no {script} script",
                    runner.name
                );
            }
        }
        assert!(root.join(E2E.args[2]).is_file());
        let config = fs::read_to_string(root.join(E2E.args[2])).unwrap();
        for project in ["fixture", "real-binaries"] {
            assert!(config.contains(&format!("name: '{project}'")), "{project}");
        }
    }
}
