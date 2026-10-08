//! `cargo xtask coverage`: the Rust, browser, WooCommerce and stagenet
//! collectors, the combined report and index, the job summary and the
//! artifact validation (`docs/COVERAGE.md`). Each collector runs its steps
//! from the repository root with `COVERAGE_OUTPUT` naming its folder,
//! everything they print going to that folder's `test.log`.

use crate::support::{at, files_under, read_json, root, write_json, Exit};
use maud::{html, Markup, PreEscaped, DOCTYPE};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fmt, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
};

pub(crate) const HELP: &str = "\
        coverage rust Refresh nightly and cargo-llvm-cov; run workspace tests and collect Rust coverage\n\
        coverage browser\n\
                      Run deterministic Playwright tests and collect authored browser source coverage\n\
        coverage woocommerce\n\
                      Run default PHPUnit tests in wp-env and collect plugin coverage\n\
        coverage stagenet\n\
                      Explicit extended run: paid browser tests and separate instrumented report\n\
        coverage all  Run rust, browser and woocommerce side by side; preserve successful reports if another fails\n\
        coverage report\n\
                      Combine rust, browser and woocommerce outputs already in target/coverage (as CI's\n\
                      separate jobs leave them) into one index; validate it once all three passed\n\
        coverage summary\n\
                      Print the coverage components as a GitHub job-summary table (Markdown)\n\
        coverage validate\n\
                      Check target/coverage: manifests, report links, screenshots, sources, line floors\n\
        coverage open Open target/coverage/index.html in the default browser";

/// A collector, as `cargo xtask coverage <name>` and run.json name it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Component {
    Rust,
    Browser,
    Woocommerce,
    Stagenet,
}

impl Component {
    fn name(self) -> &'static str {
        match self {
            Component::Rust => "rust",
            Component::Browser => "browser",
            Component::Woocommerce => "woocommerce",
            Component::Stagenet => "stagenet",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        [
            Component::Rust,
            Component::Browser,
            Component::Woocommerce,
            Component::Stagenet,
        ]
        .into_iter()
        .find(|c| c.name() == name)
    }
}

impl fmt::Display for Component {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The collectors `all` runs, in the order the index and validation expect.
const ALL: [Component; 3] = [Component::Rust, Component::Browser, Component::Woocommerce];

/// How a collector ended; `unavailable` is a collector that left no result.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Status {
    Passed,
    Failed,
    Unavailable,
}

impl Status {
    fn name(self) -> &'static str {
        match self {
            Status::Passed => "passed",
            Status::Failed => "failed",
            Status::Unavailable => "unavailable",
        }
    }
}

/// A collector's result, kept in `<component>/result.json` and run.json.
#[derive(Clone, Serialize, Deserialize)]
struct Collected {
    component: Component,
    status: Status,
    exit_code: Option<Exit>,
    log: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Serialize)]
struct Run<'a> {
    revision: &'a str,
    components: &'a [Collected],
}

/// Covered and total counts of one metric, as every manifest records them.
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub(crate) struct Counts {
    pub(crate) covered: u64,
    pub(crate) total: u64,
}

impl Counts {
    fn add(&mut self, other: Counts) {
        self.covered += other.covered;
        self.total += other.total;
    }
}

/// One source file's figures and annotated page, in rust-crates.json.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct FileCoverage {
    /// Relative to the crate, as the crate page shows it.
    pub(crate) path: String,
    pub(crate) lines: Counts,
    pub(crate) branches: Counts,
    /// The annotated source, relative to the artifact.
    pub(crate) report: String,
}

/// One workspace crate's entry in rust-crates.json.
#[derive(Serialize, Deserialize)]
pub(crate) struct CrateCoverage {
    pub(crate) component: String,
    pub(crate) lines: Counts,
    pub(crate) branches: Counts,
    pub(crate) report: String,
    pub(crate) measured_files: usize,
    /// Source files no test profile instrumented, relative to the repository.
    pub(crate) unavailable_files: Vec<String>,
    pub(crate) files: Vec<FileCoverage>,
}

/// A collector's log, and the folder its report goes to.
struct Log {
    file: fs::File,
    output: PathBuf,
}

impl Log {
    fn new(path: &Path, output: &Path) -> io::Result<Self> {
        Ok(Log {
            file: fs::File::create(path)?,
            output: output.to_path_buf(),
        })
    }

    fn say(&self, line: &str) {
        let _ = writeln!(&self.file, "{line}");
    }

    /// A step's exit code; everything it prints goes to the log. It runs from
    /// the repository root unless `cwd` says otherwise, with `env` added.
    fn run(
        &self,
        program: &str,
        args: &[&str],
        cwd: Option<&Path>,
        env: &[(&str, String)],
    ) -> io::Result<Exit> {
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(cwd.map_or_else(root, Path::to_path_buf));
        for (key, value) in env {
            command.env(key, value);
        }
        self.run_command(command, program)
    }

    /// A prepared step's exit code, with `COVERAGE_OUTPUT` naming the
    /// collector's folder and its output going to the log. A program that
    /// cannot start is logged as a step that failed.
    fn run_command(&self, mut command: Command, name: &str) -> io::Result<Exit> {
        command
            .env("COVERAGE_OUTPUT", &self.output)
            .stdout(Stdio::from(self.file.try_clone()?))
            .stderr(Stdio::from(self.file.try_clone()?));
        match command.status() {
            Ok(status) => Ok(Exit::of(status)),
            Err(e) => {
                self.say(&format!("{name}: {e}"));
                Ok(Exit::NOT_FOUND)
            }
        }
    }

    /// A step's output, for the steps whose answer the next one needs.
    fn read(&self, program: &str, args: &[&str]) -> io::Result<String> {
        let output = Command::new(program)
            .args(args)
            .current_dir(root())
            .stderr(Stdio::from(self.file.try_clone()?))
            .output()?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// `MISSING` and why, when a prerequisite is missing.
    fn missing(&self, what: &str) -> Exit {
        self.say(&format!("missing prerequisite: {what}"));
        MISSING
    }
}

/// A collector's code when a prerequisite is missing, as distinct from
/// tests that failed.
const MISSING: Exit = Exit::new(2);

/// Whether `program` is on the PATH.
fn have(program: &str) -> bool {
    let names = if cfg!(windows) {
        vec![format!("{program}.exe"), program.to_string()]
    } else {
        vec![program.to_string()]
    };
    env::var_os("PATH").is_some_and(|path| {
        env::split_paths(&path).any(|dir| names.iter().any(|n| dir.join(n).is_file()))
    })
}

/// Stops the collector at the first failed step, as `set -e` did.
macro_rules! step {
    ($e:expr) => {{
        let exit = $e?;
        if !exit.succeeded() {
            return Ok(exit);
        }
    }};
}

/// Moves a report generator's `html/` folder's contents up into the
/// collector's folder, so the public report path is stable.
fn lift_html(output: &Path) -> io::Result<()> {
    let html = output.join("html");
    for entry in fs::read_dir(&html)? {
        let entry = entry?;
        fs::rename(entry.path(), output.join(entry.file_name()))?;
    }
    fs::remove_dir(html)
}

fn nonempty(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.len() > 0)
}

/// Rust: workspace tests under cargo-llvm-cov and nextest, with branch
/// coverage; the `JUnit` report is kept for the job summary either way.
fn collect_rust(log: &Log) -> io::Result<Exit> {
    for program in ["rustup", "cargo"] {
        if !have(program) {
            return Ok(log.missing(program));
        }
    }
    // cargo install checks crates.io and replaces an older release without
    // pinning the collector version. CI installs the same tools prebuilt and
    // sets COVERAGE_TOOLS_PREINSTALLED=1: building them here takes a minute.
    if env::var("COVERAGE_TOOLS_PREINSTALLED").as_deref() != Ok("1") {
        step!(log.run("rustup", &["update", "nightly"], None, &[]));
        step!(log.run(
            "rustup",
            &[
                "component",
                "add",
                "llvm-tools-preview",
                "--toolchain",
                "nightly"
            ],
            None,
            &[]
        ));
        step!(log.run(
            "cargo",
            &["install", "cargo-llvm-cov", "--locked"],
            None,
            &[]
        ));
        step!(log.run(
            "cargo",
            &["install", "cargo-nextest", "--locked"],
            None,
            &[]
        ));
    }
    step!(log.run("rustc", &["+nightly", "--version"], None, &[]));
    step!(log.run("cargo", &["+nightly", "--version"], None, &[]));
    step!(log.run("cargo", &["llvm-cov", "--version"], None, &[]));
    step!(log.run("cargo", &["nextest", "--version"], None, &[]));
    let output = log.output.display().to_string();
    let target =
        env::var("CARGO_TARGET_DIR").map_or_else(|_| root().join("target"), |t| root().join(t));
    let junit = target.join("nextest/ci/junit.xml");
    let _ = fs::remove_file(&junit);
    // Both reports use the same profile data; the report never reruns tests.
    // nextest (.config/nextest.toml's ci profile) runs the test binaries side
    // by side and leaves the JUnit report.
    let tests = log.run(
        "cargo",
        &[
            "+nightly",
            "llvm-cov",
            "nextest",
            "--workspace",
            "--locked",
            "--branch",
            "--html",
            "--output-dir",
            &output,
            "--exclude",
            "xtask",
            "--profile",
            "ci",
        ],
        None,
        &[],
    )?;
    if junit.is_file() {
        fs::copy(&junit, log.output.join("junit.xml"))?;
    }
    if !tests.succeeded() {
        return Ok(tests);
    }
    let raw = log.output.join("raw.json").display().to_string();
    step!(log.run(
        "cargo",
        &[
            "+nightly",
            "llvm-cov",
            "report",
            "--branch",
            "--json",
            "--output-path",
            &raw
        ],
        None,
        &[]
    ));
    lift_html(&log.output)?;
    Ok(Exit::passed(nonempty(&log.output.join("index.html"))))
}

/// The browser and Playwright setup both browser collectors need; what is
/// missing, as the collector's exit.
fn browser_prerequisites(log: &Log) -> Option<Exit> {
    for program in ["node", "cargo"] {
        if !have(program) {
            return Some(log.missing(program));
        }
    }
    if !root()
        .join("e2e/browser/node_modules/@playwright/test/package.json")
        .is_file()
    {
        return Some(log.missing("e2e/browser/node_modules (run npm ci there)"));
    }
    None
}

fn playwright(log: &Log, config: &str, env: &[(&str, String)]) -> io::Result<Exit> {
    let dir = root().join("e2e/browser");
    log.run(
        &dir.join("node_modules/.bin/playwright")
            .display()
            .to_string(),
        &["test", "-c", config],
        Some(&dir),
        env,
    )
}

/// Browser: the deterministic suite against a fixture server, then the same
/// code against the real engine and monokulo binaries (and a fake monerod),
/// both instrumented and photographed.
fn collect_browser(log: &Log) -> io::Result<Exit> {
    if let Some(exit) = browser_prerequisites(log) {
        return Ok(exit);
    }
    if !root()
        .join("crates/monokulo/pos-ui/node_modules/vite/bin/vite.js")
        .is_file()
    {
        return Ok(log.missing("POS Vite dependencies (run npm ci in crates/monokulo/pos-ui)"));
    }
    // Build the fixture server up front: each spec's beforeAll rebuilds it,
    // and a cold build there overruns the 40s hook timeout.
    step!(log.run(
        "cargo",
        &[
            "build",
            "--locked",
            "-p",
            "monokulo",
            "--example",
            "coverage_fixture"
        ],
        None,
        &[]
    ));
    step!(log.run(
        "node",
        &["e2e/browser/prepare-coverage-assets.js"],
        None,
        &[]
    ));
    let raw = log.output.join("raw");
    let env = [
        (
            "COVERAGE_ASSETS_DIR",
            log.output.join("assets").display().to_string(),
        ),
        ("COVERAGE_RAW_DIR", raw.display().to_string()),
        ("COVERAGE_INSTRUMENT", "1".to_string()),
        ("COVERAGE_SCREENSHOTS", "1".to_string()),
    ];
    let screenshots = log
        .output
        .parent()
        .unwrap_or(Path::new(""))
        .join("screenshots");
    let _ = fs::remove_dir_all(&screenshots);
    fs::create_dir_all(&raw)?;
    // The real-binaries suite runs even when the first failed, so one run
    // reports every failure. It adds the Logs page, the POS session timeline
    // and store Diagnostics to the gallery.
    let first = playwright(log, "coverage-browser.config.js", &env)?;
    let second = playwright(log, "coverage-real-binaries.config.js", &env)?;
    if !first.succeeded() {
        return Ok(first);
    }
    if !second.succeeded() {
        return Ok(second);
    }
    log.run(
        "node",
        &["e2e/browser/collect-browser-report.js"],
        None,
        &env,
    )
}

/// Stagenet: the paid browser tests, with a separate instrumented report.
fn collect_stagenet(log: &Log) -> io::Result<Exit> {
    if let Some(exit) = browser_prerequisites(log) {
        return Ok(exit);
    }
    step!(log.run(
        "node",
        &["e2e/browser/prepare-coverage-assets.js"],
        None,
        &[]
    ));
    let raw = log.output.join("raw");
    let env = [
        ("COVERAGE_PROFILE", "stagenet".to_string()),
        ("COVERAGE_INSTRUMENT", "1".to_string()),
        ("COVERAGE_SCREENSHOTS", "1".to_string()),
        ("COVERAGE_RAW_DIR", raw.display().to_string()),
    ];
    fs::create_dir_all(&raw)?;
    step!(playwright(log, "coverage-stagenet.config.js", &env));
    log.run(
        "node",
        &["e2e/browser/collect-browser-report.js"],
        None,
        &env,
    )
}

/// The running wp-env tests-wordpress container that mounts this plugin.
fn wordpress_container(log: &Log, plugin: &str) -> io::Result<Option<String>> {
    if let Ok(named) = env::var("COVERAGE_WP_CONTAINER") {
        if !named.is_empty() {
            return Ok(Some(named));
        }
    }
    for candidate in log
        .read(
            "docker",
            &[
                "ps",
                "--filter",
                "name=tests-wordpress",
                "--format",
                "{{.Names}}",
            ],
        )?
        .lines()
    {
        let mounts = log.read(
            "docker",
            &[
                "inspect",
                candidate,
                "--format",
                "{{range .Mounts}}{{.Source}}{{\"\\n\"}}{{end}}",
            ],
        )?;
        if mounts.lines().any(|m| m == plugin) {
            return Ok(Some(candidate.to_string()));
        }
    }
    Ok(None)
}

/// The test image: the wp-env container's image with Xdebug added
/// (plugins/woocommerce/coverage.Dockerfile, fed on stdin).
fn build_php_image(log: &Log, plugin_dir: &Path, container: &str) -> io::Result<Exit> {
    let base_image = log
        .read(
            "docker",
            &["inspect", container, "--format", "{{.Config.Image}}"],
        )?
        .trim()
        .to_string();
    let dockerfile = plugin_dir.join("coverage.Dockerfile");
    let mut build = Command::new("docker");
    build
        .args([
            "build",
            "-q",
            "-t",
            "monokulo-coverage-php:local",
            "--build-arg",
            &format!("BASE_IMAGE={base_image}"),
            "-",
        ])
        .current_dir(root())
        .stdin(Stdio::from(
            fs::File::open(&dockerfile).map_err(|e| at(&dockerfile, e))?,
        ));
    log.run_command(build, "docker build")
}

/// PHP in the test image, sharing the wp-env container's volumes, network
/// and database settings, as the caller's own user so the report is theirs.
struct Php {
    user: String,
    container: String,
    network: String,
    mount: String,
    database: Vec<String>,
}

impl Php {
    fn new(log: &Log, container: String) -> io::Result<Self> {
        let database = log
            .read(
                "docker",
                &[
                    "inspect",
                    &container,
                    "--format",
                    "{{range .Config.Env}}{{println .}}{{end}}",
                ],
            )?
            .lines()
            .filter(|l| l.starts_with("WORDPRESS_DB_"))
            .map(str::to_string)
            .collect();
        Ok(Php {
            user: log.read("id", &["-u"])?.trim().to_string()
                + ":"
                + log.read("id", &["-g"])?.trim(),
            network: format!("container:{container}"),
            mount: format!("{}:/coverage", log.output.display()),
            container,
            database,
        })
    }

    fn run(&self, log: &Log, args: &[&str]) -> io::Result<Exit> {
        let mut all: Vec<&str> = vec![
            "run",
            "--rm",
            "--user",
            &self.user,
            "--volumes-from",
            &self.container,
            "--network",
            &self.network,
            "-v",
            &self.mount,
            "-w",
            "/var/www/html/wp-content/plugins/monokulo",
            "-e",
            "WP_TESTS_DIR=/wordpress-phpunit",
            "-e",
            "XDEBUG_MODE=coverage",
        ];
        for item in &self.database {
            all.extend(["-e", item]);
        }
        all.push("monokulo-coverage-php:local");
        all.extend(args);
        log.run("docker", &all, None, &[])
    }
}

/// WooCommerce: the plugin's `PHPUnit` suite with path coverage, in an
/// Xdebug image derived from the running `wp-env` test container.
fn collect_woocommerce(log: &Log) -> io::Result<Exit> {
    if !have("docker") {
        return Ok(log.missing("docker"));
    }
    let plugin_dir = root().join("plugins/woocommerce");
    if !plugin_dir.join("vendor/bin/phpunit").is_file() {
        return Ok(log.missing(
            "WooCommerce vendor/bin/phpunit (run composer install in plugins/woocommerce)",
        ));
    }
    let plugin = plugin_dir.display().to_string();
    let Some(container) = wordpress_container(log, &plugin)? else {
        return Ok(log.missing("running wp-env tests-wordpress container mounting this plugin"));
    };
    step!(build_php_image(log, &plugin_dir, &container));
    let php = Php::new(log, container)?;
    if !php
        .run(
            log,
            &[
                "php",
                "-r",
                r#"exit(extension_loaded("xdebug") && in_array("coverage", xdebug_info("mode"), true) ? 0 : 1);"#,
            ],
        )?
        .succeeded()
    {
        return Ok(log.missing("Xdebug coverage mode in derived tests-cli image"));
    }
    step!(php.run(
        log,
        &[
            "php",
            "-dauto_prepend_file=/var/www/html/wp-content/plugins/monokulo/coverage-filter.php",
            "vendor/bin/phpunit",
            "--configuration",
            "phpunit.coverage.xml",
            "--path-coverage",
            "--coverage-html",
            "/coverage/html",
            "--coverage-xml",
            "/coverage/xml",
            "--coverage-clover",
            "/coverage/clover.xml",
            "--coverage-php",
            "/coverage/coverage.php",
            "--log-junit",
            "/coverage/junit.xml",
        ]
    ));
    step!(php.run(log, &["php", "coverage-summary.php"]));
    lift_html(&log.output)?;
    let complete =
        nonempty(&log.output.join("index.html")) && nonempty(&log.output.join("xml/index.xml"));
    Ok(Exit::passed(complete))
}

/// Runs one collector and leaves its result in `<component>/result.json`, so
/// `report` can combine collectors that ran separately.
fn run(component: Component, output: &Path) -> io::Result<Collected> {
    let result = run_collector(component, output)?;
    write_json(&output.join(component.name()).join("result.json"), &result)?;
    Ok(result)
}

fn run_collector(component: Component, output: &Path) -> io::Result<Collected> {
    let dir = output.join(component.name());
    let manifest_path = output.join(format!("{component}.json"));
    if manifest_path.exists() {
        fs::remove_file(&manifest_path)?;
    }
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    fs::create_dir_all(&dir)?;
    let log_path = dir.join("test.log");
    let log = Log::new(&log_path, &dir)?;
    eprintln!(
        "coverage {component}: collecting (log: {})",
        log_path.display()
    );
    let exit = match component {
        Component::Rust => collect_rust(&log),
        Component::Browser => collect_browser(&log),
        Component::Woocommerce => collect_woocommerce(&log),
        Component::Stagenet => collect_stagenet(&log),
    }?;
    let mut passed = exit.succeeded();
    if passed {
        let checked = match component {
            Component::Rust => summarize_rust(output),
            Component::Woocommerce => summarize_woocommerce(output),
            Component::Stagenet => stagenet_reports(output),
            Component::Browser => Ok(()),
        };
        if let Err(e) = checked {
            eprintln!("coverage {component}: report validation failed: {e}");
            passed = false;
        }
    }
    eprintln!(
        "coverage {component}: {} (log: {})",
        if passed { "passed" } else { "failed" },
        log_path.display()
    );
    Ok(Collected {
        component,
        status: if passed {
            Status::Passed
        } else {
            Status::Failed
        },
        exit_code: Some(exit),
        log: format!("{component}/test.log"),
        reason: None,
    })
}

/// The stagenet collector leaves no manifest of its own; its reports must.
fn stagenet_reports(output: &Path) -> io::Result<()> {
    for required in [
        "stagenet.json",
        "stagenet/index.html",
        "stagenet/screenshots/index.html",
    ] {
        if !output.join(required).is_file() {
            return Err(io::Error::other(format!("missing report {required}")));
        }
    }
    Ok(())
}

fn version(command: &str, args: &[&str]) -> io::Result<String> {
    // In the workspace, whatever directory xtask was started from: `git`
    // answers about the repository it is run in.
    let output = Command::new(command)
        .args(args)
        .current_dir(root())
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{command} {} failed",
            args.join(" ")
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The checkout's revision, and whether it had uncommitted changes.
fn revision() -> io::Result<(String, bool)> {
    let revision = version("git", &["rev-parse", "HEAD"])?;
    let dirty = !Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(root())
        .output()?
        .stdout
        .is_empty();
    Ok((revision, dirty))
}

/// `covered` and `count` of one metric in llvm-cov's JSON.
fn llvm_counts(summary: &Value, metric: &str) -> io::Result<Counts> {
    let value = &summary[metric];
    let field = |name: &str| {
        value[name]
            .as_u64()
            .ok_or_else(|| io::Error::other(format!("missing {metric}.{name}")))
    };
    Ok(Counts {
        covered: field("covered")?,
        total: field("count")?,
    })
}

fn summarize_rust(output: &Path) -> io::Result<()> {
    let raw: Value = read_json(&output.join("rust/raw.json"))?;
    let data = raw["data"]
        .as_array()
        .and_then(|a| a.first())
        .ok_or_else(|| io::Error::other("Rust JSON has no data"))?;
    let lines = llvm_counts(&data["totals"], "lines")?;
    let branches = llvm_counts(&data["totals"], "branches")?;
    if lines.total == 0 || branches.total == 0 {
        return Err(io::Error::other("Rust line or branch denominator is zero"));
    }
    let files = data["files"]
        .as_array()
        .ok_or_else(|| io::Error::other("Rust JSON has no files"))?;
    for crate_name in ["engine", "monokulo"] {
        let branch_count: u64 = files
            .iter()
            .filter(|f| {
                f["filename"]
                    .as_str()
                    .is_some_and(|s| s.contains(&format!("/crates/{crate_name}/src/")))
            })
            .filter_map(|f| f["summary"]["branches"]["count"].as_u64())
            .sum();
        if branch_count == 0 {
            return Err(io::Error::other(format!(
                "missing branch data for {crate_name}"
            )));
        }
    }
    let (revision, source_dirty) = revision()?;
    let manifest = serde_json::json!({
        "component":"rust-workspace", "revision":revision, "source_dirty":source_dirty,
        "tools": {"rustc":version("rustc", &["+nightly", "--version"])?,
            "cargo":version("cargo", &["+nightly", "--version"])?,
            "collector":version("cargo", &["llvm-cov", "--version"])?},
        "test":{"status":"passed", "command":"cargo +nightly llvm-cov nextest --workspace --locked --branch --html --exclude xtask --profile ci",
            "exit_code":0,"log":"rust/test.log"},
        "lines": lines, "branches": branches,
        "report":"rust/index.html", "unavailable":[]
    });
    write_json(&output.join("rust.json"), &manifest)?;
    write_rust_crates(output, files, lines, branches)
}

fn summarize_woocommerce(output: &Path) -> io::Result<()> {
    let dir = output.join("woocommerce");
    let summary: Value = read_json(&dir.join("summary.json"))?;
    let counts = |name: &str| -> io::Result<Counts> {
        let value = &summary[name];
        let covered = value["covered"]
            .as_u64()
            .ok_or_else(|| io::Error::other(format!("missing PHP {name}.covered")))?;
        let total = value["total"]
            .as_u64()
            .ok_or_else(|| io::Error::other(format!("missing PHP {name}.total")))?;
        if total == 0 || covered > total {
            return Err(io::Error::other(format!("invalid PHP {name} counts")));
        }
        Ok(Counts { covered, total })
    };
    let lines = counts("lines")?;
    let branches = counts("branches")?;
    counts("paths")?;
    let files = summary["files"]
        .as_array()
        .ok_or_else(|| io::Error::other("missing PHP file list"))?;
    let expected = ["monokulo.php", "class-wc-gateway-monokulo.php"];
    if files.len() != expected.len()
        || expected
            .iter()
            .any(|name| !files.iter().any(|f| f["name"].as_str() == Some(name)))
    {
        return Err(io::Error::other(
            "PHP coverage contains missing or non-plugin source files",
        ));
    }
    for required in [
        "index.html",
        "xml/index.xml",
        "clover.xml",
        "junit.xml",
        "includes/class-wc-gateway-monokulo.php_branch.html",
    ] {
        if !dir.join(required).is_file() {
            return Err(io::Error::other(format!("missing PHP report {required}")));
        }
    }
    let (revision, source_dirty) = revision()?;
    let versions = &summary["versions"];
    let named = |tool: &str| {
        versions[tool]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| io::Error::other(format!("missing {tool} version")))
    };
    let (php, phpunit, xdebug) = (named("php")?, named("phpunit")?, named("xdebug")?);
    let manifest = serde_json::json!({
        "component":"woocommerce", "revision":revision, "source_dirty":source_dirty,
        "tools":{"rustc":version("rustc", &["--version"])?, "cargo":version("cargo", &["--version"])?,
            "collector":format!("PHPUnit {phpunit} / Xdebug {xdebug}"), "php":php},
        "test":{"status":"passed", "command":"vendor/bin/phpunit --configuration phpunit.coverage.xml --path-coverage",
            "exit_code":0, "log":"woocommerce/test.log"},
        "lines": lines, "branches": branches,
        "report":"woocommerce/index.html", "unavailable":[]
    });
    write_json(&output.join("woocommerce.json"), &manifest)
}

/// A crate's Rust sources, canonical, leaving out its test modules.
fn source_files(src: &Path) -> io::Result<BTreeSet<PathBuf>> {
    let mut found = BTreeSet::new();
    for path in files_under(src, |_| false)? {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if path.extension().is_some_and(|e| e == "rs")
            && name != "tests.rs"
            && !name.ends_with("_tests.rs")
        {
            found.insert(path.canonicalize().map_err(|e| at(&path, e))?);
        }
    }
    Ok(found)
}

/// Every product crate of the workspace and its folder, from cargo metadata.
fn workspace_crates() -> io::Result<BTreeMap<String, PathBuf>> {
    let metadata_run = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1", "--locked"])
        .current_dir(root())
        .output()?;
    if !metadata_run.status.success() {
        return Err(io::Error::other(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&metadata_run.stderr)
        )));
    }
    let metadata: Value = serde_json::from_slice(&metadata_run.stdout)?;
    let members: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .ok_or_else(|| io::Error::other("Cargo metadata has no workspace members"))?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let mut crates = BTreeMap::new();
    for package in metadata["packages"]
        .as_array()
        .ok_or_else(|| io::Error::other("Cargo metadata has no packages"))?
    {
        if !package["id"]
            .as_str()
            .is_some_and(|id| members.contains(id))
        {
            continue;
        }
        let name = package["name"]
            .as_str()
            .ok_or_else(|| io::Error::other("package without name"))?;
        if name == "xtask" {
            continue;
        }
        let manifest = Path::new(
            package["manifest_path"]
                .as_str()
                .ok_or_else(|| io::Error::other("package without manifest path"))?,
        );
        let dir = manifest
            .parent()
            .ok_or_else(|| io::Error::other(format!("{name}: manifest path has no folder")))?;
        crates.insert(name.to_owned(), dir.canonicalize().map_err(|e| at(dir, e))?);
    }
    if crates.is_empty() {
        return Err(io::Error::other("no product workspace crates"));
    }
    Ok(crates)
}

/// One crate's figures from llvm-cov's per-file data.
fn crate_coverage(
    name: &str,
    dir: &Path,
    files: &[Value],
    output: &Path,
    repo: &Path,
) -> io::Result<CrateCoverage> {
    let src = dir.join("src");
    let all_source = if src.exists() {
        source_files(&src)?
    } else {
        BTreeSet::new()
    };
    let relative_to = |base: &Path, path: &Path| -> io::Result<String> {
        path.strip_prefix(base)
            .map(|p| p.display().to_string())
            .map_err(|_| {
                io::Error::other(format!("{} is outside {}", path.display(), base.display()))
            })
    };
    let mut measured = BTreeSet::new();
    let mut lines = Counts::default();
    let mut branches = Counts::default();
    let mut rows = Vec::new();
    for file in files {
        let Some(filename) = file["filename"].as_str() else {
            continue;
        };
        let path = Path::new(filename);
        if !path.starts_with(&src) || path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        measured.insert(path.to_path_buf());
        let l = llvm_counts(&file["summary"], "lines")?;
        let b = llvm_counts(&file["summary"], "branches")?;
        lines.add(l);
        branches.add(b);
        let report = format!("rust/coverage/{}.html", filename.trim_start_matches('/'));
        if !output.join(&report).is_file() {
            return Err(io::Error::other(format!(
                "missing annotated source: {report}"
            )));
        }
        rows.push(FileCoverage {
            path: relative_to(dir, path)?,
            lines: l,
            branches: b,
            report,
        });
    }
    let unavailable_files = all_source
        .difference(&measured)
        .map(|p| relative_to(repo, p))
        .collect::<io::Result<Vec<_>>>()?;
    Ok(CrateCoverage {
        component: name.to_owned(),
        lines,
        branches,
        report: format!("rust/crates/{name}.html"),
        measured_files: measured.len(),
        unavailable_files,
        files: rows,
    })
}

/// `covered/total`, as the pages show a metric.
fn ratio(counts: Counts) -> String {
    format!("{}/{}", counts.covered, counts.total)
}

/// The crate's page in rust/crates/: its files with their figures, each
/// linked to its annotated source.
fn crate_page(c: &CrateCoverage) -> Markup {
    html! {
        (DOCTYPE)
        meta charset="utf-8";
        title { (c.component) " Rust coverage" }
        h1 { (c.component) }
        p { a href="index.html" { "All crates" } " · " a href="../index.html" { "LLVM report" } }
        table border="1" {
            tr { th { "Source" } th { "Lines" } th { "Branches" } }
            @for f in &c.files {
                // The page sits in rust/crates/, one folder below the annotated sources.
                @let href = f.report.strip_prefix("rust/").unwrap_or(&f.report);
                tr { td { a href={ "../" (href) } { (f.path) } } td { (ratio(f.lines)) } td { (ratio(f.branches)) } }
            }
            @for missing in &c.unavailable_files {
                @let relative = missing.strip_prefix(&format!("crates/{}/", c.component)).unwrap_or(missing);
                tr { td { (relative) } td colspan="2" { "unavailable: no executable code in this profile or feature gated" } }
            }
        }
    }
}

/// The crates' index page in rust/crates/.
fn crates_page(crates: &[CrateCoverage]) -> Markup {
    html! {
        (DOCTYPE)
        meta charset="utf-8";
        title { "Rust crates" }
        h1 { "Rust coverage by crate" }
        table border="1" {
            tr { th { "Crate" } th { "Lines" } th { "Branches" } th { "Source files" } }
            @for c in crates {
                tr {
                    td { a href={ (c.component) ".html" } { (c.component) } }
                    td { (ratio(c.lines)) }
                    td { (ratio(c.branches)) }
                    td { (c.measured_files) " measured, " (c.unavailable_files.len()) " unavailable" }
                }
            }
        }
        p { "Unavailable files have no executable code in this build or require a feature not enabled by the default test run." }
    }
}

/// Per-crate pages and rust-crates.json from llvm-cov's per-file data, whose
/// totals must add up to the workspace figures.
fn write_rust_crates(
    output: &Path,
    files: &[Value],
    workspace_lines: Counts,
    workspace_branches: Counts,
) -> io::Result<()> {
    let repo = root().canonicalize().map_err(|e| at(&root(), e))?;
    let pages = output.join("rust/crates");
    fs::create_dir_all(&pages)?;
    let mut summaries = Vec::new();
    for (name, dir) in workspace_crates()? {
        let c = crate_coverage(&name, &dir, files, output, &repo)?;
        fs::write(
            pages.join(format!("{name}.html")),
            crate_page(&c).into_string(),
        )?;
        summaries.push(c);
    }
    let mapped: usize = summaries.iter().map(|c| c.measured_files).sum();
    if mapped != files.len() {
        return Err(io::Error::other(format!(
            "{} Rust files did not map to a workspace crate",
            files.len() - mapped
        )));
    }
    let (mut lines, mut branches) = (Counts::default(), Counts::default());
    for c in &summaries {
        lines.add(c.lines);
        branches.add(c.branches);
    }
    for (name, sum, whole) in [
        ("lines", lines, workspace_lines),
        ("branches", branches, workspace_branches),
    ] {
        if (sum.covered, sum.total) != (whole.covered, whole.total) {
            return Err(io::Error::other(format!(
                "crate {name} totals differ from LLVM workspace totals"
            )));
        }
    }
    fs::write(
        pages.join("index.html"),
        crates_page(&summaries).into_string(),
    )?;
    write_json(&output.join("rust-crates.json"), &summaries)
}

/// A component's row of the index: its status and log, counts and report.
fn component_row(output: &Path, result: &Collected, manifest: Option<&Value>) -> Markup {
    let name = result.component.name();
    let count = |metric: &str| -> String {
        manifest
            .and_then(|m| {
                Some(format!(
                    "{} / {}",
                    m[metric]["covered"].as_u64()?,
                    m[metric]["total"].as_u64()?
                ))
            })
            .unwrap_or_else(|| "unavailable".into())
    };
    let report = manifest
        .and_then(|m| m["report"].as_str())
        .filter(|p| output.join(p).is_file());
    html! {
        tr {
            td { (name) }
            td class=(if result.status == Status::Passed { "good" } else { "bad" }) {
                (result.status.name()) " " a href={ (name) "/test.log" } { "log" }
            }
            td { (count("lines")) }
            td { (count("branches")) }
            td {
                @if let Some(path) = report { a href=(path) { "Annotated source" } }
                @else { "unavailable" }
            }
        }
    }
}

/// One picture per page or feature, in the gallery's order, at desktop
/// size where there is one.
fn screenshot_strip(output: &Path) -> io::Result<Markup> {
    let entries: Value = read_json(&output.join("screenshots/manifest.json"))?;
    let entries = entries
        .as_array()
        .ok_or_else(|| io::Error::other("screenshot manifest is not an array"))?;
    let mut groups: Vec<&str> = Vec::new();
    for group in entries.iter().filter_map(|e| e["group"].as_str()) {
        if !groups.contains(&group) {
            groups.push(group);
        }
    }
    // Each group's picture: its stage and the image, checked to exist.
    let mut pictures: Vec<(&str, &str, String)> = Vec::new();
    for group in groups {
        let shown = |e: &&Value| e["group"] == group && e["stage"] != "failure";
        let desktop = entries
            .iter()
            .filter(shown)
            .find(|e| e["shape"] == "desktop");
        if let Some(entry) = desktop.or_else(|| entries.iter().find(shown)) {
            let image = entry["image"]
                .as_str()
                .ok_or_else(|| io::Error::other("screenshot entry has no image"))?;
            if !output.join("screenshots").join(image).is_file() {
                return Err(io::Error::other(format!("missing screenshot: {image}")));
            }
            let stage = entry["stage"].as_str().unwrap_or(group);
            pictures.push((group, stage, format!("screenshots/{image}")));
        }
    }
    Ok(html! {
        h2 { "UI evidence" }
        div style="display:flex;flex-wrap:wrap;gap:1rem" {
            @for (group, stage, source) in &pictures {
                a href=(source) style="width:30%;min-width:220px" {
                    img src=(source) alt=(group) style="width:100%;height:160px;object-fit:contain;background:#eee";
                    br;
                    (stage)
                }
            }
        }
    })
}

const INDEX_STYLE: &str = "body{font:16px/1.5 system-ui,sans-serif;max-width:1050px;margin:2rem auto;padding:0 1rem;color:#17212b}table{border-collapse:collapse;width:100%}th,td{border-bottom:1px solid #ccd3db;text-align:left;padding:.65rem}code{overflow-wrap:anywhere}a{color:#164e8a}.bad{color:#a32}.good{color:#175c38}small{color:#52606d}";

fn render_index(output: &Path, results: &[Collected]) -> io::Result<()> {
    // Each component's manifest, read once for its row, the revision and the toolchains.
    let manifests: Vec<Option<Value>> = results
        .iter()
        .map(|r| {
            let path = output.join(format!("{}.json", r.component));
            if path.is_file() {
                read_json(&path).map(Some)
            } else {
                Ok(None)
            }
        })
        .collect::<io::Result<_>>()?;
    let rows = results.iter().zip(&manifests);
    let strip = if output.join("screenshots/index.html").is_file() {
        Some(screenshot_strip(output)?)
    } else {
        None
    };
    let tool = |m: &Value, name: &str| m["tools"][name].as_str().unwrap_or("?").to_string();
    let page = html! {
        (DOCTYPE)
        html lang="en" {
            meta charset="utf-8";
            meta name="viewport" content="width=device-width";
            title { "Monokulo coverage" }
            style { (PreEscaped(INDEX_STYLE)) }
            h1 { "Monokulo coverage" }
            p { "Coverage from separate Rust, browser, and WooCommerce test runs. Counts use executable lines and branches in authored product source." }
            table {
                thead { tr { th { "Component" } th { "Tests" } th { "Lines" } th { "Branches" } th { "Reports" } } }
                tbody { @for (result, manifest) in rows.clone() { (component_row(output, result, manifest.as_ref())) } }
            }
            @if output.join("stress/index.html").is_file() {
                p { a href="stress/index.html" { "Engine stress report" } }
            }
            @if output.join("rust/crates/index.html").is_file() {
                p { a href="rust/crates/index.html" { "Rust coverage by crate and source file" } }
            }
            @if let Some(strip) = strip {
                p { a href="screenshots/index.html" { "Screenshot gallery" } }
                (strip)
            }
            @if let Some(first) = manifests.iter().flatten().next() {
                p { small {
                    "Revision: " code { (first["revision"].as_str().unwrap_or("unknown")) }
                    @if first["source_dirty"] == true { " · source tree dirty during collection" }
                } }
            }
            h2 { "Toolchains" }
            ul {
                @for (result, manifest) in rows {
                    @if let Some(m) = manifest {
                        li { (result.component) ": " code { (tool(m, "rustc")) } "; " code { (tool(m, "cargo")) } "; " code { (tool(m, "collector")) } }
                    }
                }
            }
        }
    };
    fs::write(output.join("index.html"), page.into_string())
}

pub(crate) fn coverage(command: &str) -> io::Result<Exit> {
    let output = root().join("target/coverage");
    if command == "open" {
        let index = output.join("index.html");
        if !index.is_file() {
            eprintln!("missing report: {}", index.display());
            return Ok(Exit::FAILURE);
        }
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        return match Command::new(opener).arg(&index).status() {
            Ok(status) => Ok(Exit::of(status)),
            Err(e) => {
                eprintln!("missing opener {opener}: {e}");
                Ok(Exit::FAILURE)
            }
        };
    }
    fs::create_dir_all(&output)?;
    let names: &[Component] = match command {
        "all" => &ALL,
        "report" => return report(&output),
        "summary" => {
            print!("{}", crate::summary::coverage_summary(&output)?);
            return Ok(Exit::SUCCESS);
        }
        "validate" => return Ok(validate()),
        _ => {
            let Some(component) = Component::parse(command) else {
                crate::help();
                return Ok(Exit::FAILURE);
            };
            &[component]
        }
    };
    let revision = version("git", &["rev-parse", "HEAD"])?;
    write_run(&output, &revision, &[])?;
    // The collectors share nothing (each has its own target directory,
    // processes and ports), so they run side by side, each logging to its
    // own test.log.
    let (done, finished) = mpsc::channel();
    let results = thread::scope(|scope| -> io::Result<Vec<Collected>> {
        for component in names {
            let (done, output) = (done.clone(), &output);
            scope.spawn(move || done.send(run(*component, output)));
        }
        drop(done);
        let mut results = Vec::new();
        for result in finished {
            results.push(result?);
            results.sort_by_key(|r| names.iter().position(|n| r.component == *n));
            // Write after every collector so an interrupted run retains progress.
            write_run(&output, &revision, &results)?;
        }
        Ok(results)
    })?;
    let passed = results.iter().all(|r| r.status == Status::Passed);
    if passed && command == "all" {
        return Ok(validate());
    }
    Ok(Exit::passed(passed))
}

/// Writes `run.json` for `results` and renders the index from it.
fn write_run(output: &Path, revision: &str, results: &[Collected]) -> io::Result<()> {
    write_json(
        &output.join("run.json"),
        &Run {
            revision,
            components: results,
        },
    )?;
    render_index(output, results)
}

/// `coverage report`: the index and validation for collectors that ran
/// separately. CI runs each in a job of its own and gathers their outputs
/// into one `target/coverage`; a collector that left no result is
/// unavailable, and the run fails.
fn report(output: &Path) -> io::Result<Exit> {
    let mut results = Vec::new();
    for component in ALL {
        let path = output.join(component.name()).join("result.json");
        results.push(if path.is_file() {
            read_json(&path)?
        } else {
            Collected {
                component,
                status: Status::Unavailable,
                exit_code: None,
                log: format!("{component}/test.log"),
                reason: Some("no result: the collector did not run or did not finish".into()),
            }
        });
    }
    // The collectors' own revision (validation rejects a mix), or this
    // checkout's when none recorded one.
    let revision = ALL.iter().find_map(|component| {
        let manifest: Value = read_json(&output.join(format!("{component}.json"))).ok()?;
        manifest["revision"].as_str().map(str::to_owned)
    });
    let revision = match revision {
        Some(r) => r,
        None => version("git", &["rev-parse", "HEAD"])?,
    };
    write_run(output, &revision, &results)?;
    for result in &results {
        eprintln!("coverage {}: {}", result.component, result.status.name());
    }
    if results.iter().all(|r| r.status == Status::Passed) {
        Ok(validate())
    } else {
        Ok(Exit::FAILURE)
    }
}

fn validate() -> Exit {
    match crate::validate::validate(&root(), &root().join("target/coverage")) {
        Ok(()) => {
            println!("coverage artifact validation passed");
            Exit::SUCCESS
        }
        Err(problem) => {
            eprintln!("coverage artifact validation failed: {problem}");
            Exit::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{html_links, Scratch};

    fn put(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// The links of a page, each checked to name a file beside it.
    fn links_resolve(page: &Path) -> Vec<String> {
        let text = fs::read_to_string(page).unwrap();
        let links = html_links(&text);
        for link in &links {
            assert!(
                page.parent().unwrap().join(link).is_file(),
                "{}: broken link {link}",
                page.display()
            );
        }
        links
    }

    #[test]
    fn the_index_links_every_report_it_has_and_escapes_what_it_shows() {
        let scratch = Scratch::new("coverage-index");
        let out = scratch.path();
        put(&out.join("rust/index.html"), "<p>llvm</p>");
        put(&out.join("rust/test.log"), "");
        put(&out.join("browser/test.log"), "");
        put(&out.join("stress/index.html"), "<p>stress</p>");
        put(&out.join("rust/crates/index.html"), "<p>crates</p>");
        put(&out.join("screenshots/index.html"), "<p>gallery</p>");
        put(&out.join("screenshots/images/paid.png"), "png");
        put(
            &out.join("screenshots/manifest.json"),
            r#"[{"group":"checkout","stage":"paid <b>","shape":"desktop","image":"images/paid.png"}]"#,
        );
        write_json(
            &out.join("rust.json"),
            &serde_json::json!({"revision": "abc<def", "source_dirty": true,
                "lines": {"covered": 9, "total": 10}, "branches": {"covered": 1, "total": 2}, "report": "rust/index.html",
                "tools": {"rustc": "rustc 1.0", "cargo": "cargo 1.0", "collector": "cargo-llvm-cov <0.9>"}}),
        )
        .unwrap();
        let results = [
            Collected {
                component: Component::Rust,
                status: Status::Passed,
                exit_code: Some(Exit::SUCCESS),
                log: "rust/test.log".into(),
                reason: None,
            },
            Collected {
                component: Component::Browser,
                status: Status::Unavailable,
                exit_code: None,
                log: "browser/test.log".into(),
                reason: Some("no result".into()),
            },
        ];
        render_index(out, &results).unwrap();
        let links = links_resolve(&out.join("index.html"));
        assert!(links.contains(&"rust/index.html".to_string()), "{links:?}");
        assert!(links.contains(&"screenshots/images/paid.png".to_string()));
        let page = fs::read_to_string(out.join("index.html")).unwrap();
        assert!(page.contains("<td>browser</td><td class=\"bad\">unavailable"));
        assert!(page.contains("<td>9 / 10</td>"));
        assert!(page.contains("abc&lt;def</code> · source tree dirty"));
        assert!(page.contains("cargo-llvm-cov &lt;0.9&gt;"));
        assert!(page.contains("paid &lt;b&gt;"));
    }

    #[test]
    fn a_crate_page_links_each_annotated_source_from_its_folder() {
        let scratch = Scratch::new("coverage-crate");
        let out = scratch.path();
        put(
            &out.join("rust/coverage/home/x/crates/engine/src/a&b.rs.html"),
            "",
        );
        let c = CrateCoverage {
            component: "engine".into(),
            lines: Counts {
                covered: 1,
                total: 2,
            },
            branches: Counts::default(),
            report: "rust/crates/engine.html".into(),
            measured_files: 1,
            unavailable_files: vec!["crates/engine/src/lib.rs".into()],
            files: vec![FileCoverage {
                path: "src/a&b.rs".into(),
                lines: Counts {
                    covered: 1,
                    total: 2,
                },
                branches: Counts::default(),
                report: "rust/coverage/home/x/crates/engine/src/a&b.rs.html".into(),
            }],
        };
        put(&out.join("rust/index.html"), "");
        put(&out.join("rust/crates/index.html"), "");
        let page = out.join("rust/crates/engine.html");
        fs::write(&page, crate_page(&c).into_string()).unwrap();
        assert_eq!(
            links_resolve(&page),
            [
                "index.html",
                "../index.html",
                "../coverage/home/x/crates/engine/src/a&b.rs.html"
            ]
        );
        let text = fs::read_to_string(&page).unwrap();
        assert!(
            text.contains(">src/a&amp;b.rs</a></td><td>1/2</td>"),
            "{text}"
        );
        assert!(text.contains("<td>src/lib.rs</td>"), "{text}");
    }
}
