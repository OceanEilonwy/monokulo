//! `cargo xtask coverage`: the Rust, browser, WooCommerce and stagenet
//! collectors, the combined report and index, the job summary and the
//! artifact validation (docs/COVERAGE.md). Each collector runs its steps from
//! the repository root with `COVERAGE_OUTPUT` naming its folder, everything
//! they print going to that folder's test.log.

use crate::{escape_html, root};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
};

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
    /// the repository root unless `cwd` says otherwise, with `COVERAGE_OUTPUT`
    /// set and `env` added.
    fn run(
        &self,
        program: &str,
        args: &[&str],
        cwd: Option<&Path>,
        env: &[(&str, String)],
    ) -> io::Result<i32> {
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(cwd.map_or_else(root, Path::to_path_buf))
            .env("COVERAGE_OUTPUT", &self.output)
            .stdout(Stdio::from(self.file.try_clone()?))
            .stderr(Stdio::from(self.file.try_clone()?));
        for (key, value) in env {
            command.env(key, value);
        }
        match command.status() {
            Ok(status) => Ok(status.code().unwrap_or(1)),
            Err(e) => {
                self.say(&format!("{program}: {e}"));
                Ok(127)
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

    /// Exit code 2 and why, when a prerequisite is missing.
    fn missing(&self, what: &str) -> io::Result<i32> {
        self.say(&format!("missing prerequisite: {what}"));
        Ok(2)
    }
}

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
        let code = $e?;
        if code != 0 {
            return Ok(code);
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
/// coverage; the JUnit report is kept for the job summary either way.
fn collect_rust(log: &Log) -> io::Result<i32> {
    for program in ["rustup", "cargo"] {
        if !have(program) {
            return log.missing(program);
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
    let status = log.run(
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
    if status != 0 {
        return Ok(status);
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
    Ok(if nonempty(&log.output.join("index.html")) {
        0
    } else {
        1
    })
}

/// The browser and Playwright setup both browser collectors need.
fn browser_prerequisites(log: &Log) -> io::Result<Option<i32>> {
    for program in ["node", "cargo"] {
        if !have(program) {
            return log.missing(program).map(Some);
        }
    }
    if !root()
        .join("e2e/browser/node_modules/@playwright/test/package.json")
        .is_file()
    {
        return log
            .missing("e2e/browser/node_modules (run npm ci there)")
            .map(Some);
    }
    Ok(None)
}

fn playwright(log: &Log, config: &str, env: &[(&str, String)]) -> io::Result<i32> {
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
fn collect_browser(log: &Log) -> io::Result<i32> {
    if let Some(code) = browser_prerequisites(log)? {
        return Ok(code);
    }
    if !root()
        .join("crates/monokulo/pos-ui/node_modules/vite/bin/vite.js")
        .is_file()
    {
        return log.missing("POS Vite dependencies (run npm ci in crates/monokulo/pos-ui)");
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
    let screenshots = log.output.parent().unwrap().join("screenshots");
    let _ = fs::remove_dir_all(&screenshots);
    fs::create_dir_all(&raw)?;
    // The real-binaries suite runs even when the first failed, so one run
    // reports every failure. It adds the Logs page, the POS session timeline
    // and store Diagnostics to the gallery.
    let first = playwright(log, "coverage-browser.config.js", &env)?;
    let second = playwright(log, "coverage-real-binaries.config.js", &env)?;
    if first != 0 {
        return Ok(first);
    }
    if second != 0 {
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
fn collect_stagenet(log: &Log) -> io::Result<i32> {
    if let Some(code) = browser_prerequisites(log)? {
        return Ok(code);
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

/// WooCommerce: the plugin's PHPUnit suite with path coverage, in an
/// Xdebug image derived from the running wp-env test container.
fn collect_woocommerce(log: &Log) -> io::Result<i32> {
    if !have("docker") {
        return log.missing("docker");
    }
    let plugin_dir = root().join("plugins/woocommerce");
    if !plugin_dir.join("vendor/bin/phpunit").is_file() {
        return log.missing(
            "WooCommerce vendor/bin/phpunit (run composer install in plugins/woocommerce)",
        );
    }
    let plugin = plugin_dir.display().to_string();
    let Some(container) = wordpress_container(log, &plugin)? else {
        return log.missing("running wp-env tests-wordpress container mounting this plugin");
    };
    let base_image = log
        .read(
            "docker",
            &["inspect", &container, "--format", "{{.Config.Image}}"],
        )?
        .trim()
        .to_string();
    let database: Vec<String> = log
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
    let mut build = Command::new("docker")
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
        .stdin(Stdio::from(fs::File::open(
            plugin_dir.join("coverage.Dockerfile"),
        )?))
        .stdout(Stdio::from(log.file.try_clone()?))
        .stderr(Stdio::from(log.file.try_clone()?))
        .spawn()?;
    step!(Ok::<i32, io::Error>(build.wait()?.code().unwrap_or(1)));
    let user = log.read("id", &["-u"])?.trim().to_string() + ":" + log.read("id", &["-g"])?.trim();
    let mount = format!("{}:/coverage", log.output.display());
    let network = format!("container:{container}");
    let php = |args: &[&str]| -> io::Result<i32> {
        let mut all: Vec<&str> = vec![
            "run",
            "--rm",
            "--user",
            &user,
            "--volumes-from",
            &container,
            "--network",
            &network,
            "-v",
            &mount,
            "-w",
            "/var/www/html/wp-content/plugins/monokulo",
            "-e",
            "WP_TESTS_DIR=/wordpress-phpunit",
            "-e",
            "XDEBUG_MODE=coverage",
        ];
        for item in &database {
            all.extend(["-e", item]);
        }
        all.push("monokulo-coverage-php:local");
        all.extend(args);
        log.run("docker", &all, None, &[])
    };
    if php(&[
        "php",
        "-r",
        r#"exit(extension_loaded("xdebug") && in_array("coverage", xdebug_info("mode"), true) ? 0 : 1);"#,
    ])? != 0
    {
        return log.missing("Xdebug coverage mode in derived tests-cli image");
    }
    step!(php(&[
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
    ]));
    step!(php(&["php", "coverage-summary.php"]));
    lift_html(&log.output)?;
    let complete =
        nonempty(&log.output.join("index.html")) && nonempty(&log.output.join("xml/index.xml"));
    Ok(if complete { 0 } else { 1 })
}

/// The collectors `all` runs, in the order the index and validation expect.
const ALL: [&str; 3] = ["rust", "browser", "woocommerce"];

/// Runs one collector and leaves its result in `<component>/result.json`, so
/// `report` can combine collectors that ran separately.
fn run(component: &str, output: &Path) -> io::Result<Value> {
    let result = run_collector(component, output)?;
    fs::write(
        output.join(component).join("result.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    Ok(result)
}

fn run_collector(component: &str, output: &Path) -> io::Result<Value> {
    let dir = output.join(component);
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
    let code = match component {
        "rust" => collect_rust(&log),
        "browser" => collect_browser(&log),
        "woocommerce" => collect_woocommerce(&log),
        "stagenet" => collect_stagenet(&log),
        _ => unreachable!("{component} is not a collector"),
    }?;
    let mut passed = code == 0;
    if passed && component == "rust" {
        if let Err(e) = summarize_rust(output) {
            eprintln!("coverage rust: report validation failed: {e}");
            passed = false;
        }
    }
    if passed && component == "woocommerce" {
        if let Err(e) = summarize_woocommerce(output) {
            eprintln!("coverage woocommerce: report validation failed: {e}");
            passed = false;
        }
    }
    if passed && component == "stagenet" {
        for required in [
            "stagenet.json",
            "stagenet/index.html",
            "stagenet/screenshots/index.html",
        ] {
            if !output.join(required).is_file() {
                eprintln!("coverage stagenet: missing report {required}");
                passed = false;
            }
        }
    }
    eprintln!(
        "coverage {component}: {} (log: {})",
        if passed { "passed" } else { "failed" },
        log_path.display()
    );
    Ok(
        json!({"component":component,"status":if passed {"passed"} else {"failed"},
        "exit_code":code,"log":format!("{component}/test.log")}),
    )
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

fn summarize_rust(output: &Path) -> io::Result<()> {
    let raw: Value = serde_json::from_slice(&fs::read(output.join("rust/raw.json"))?)?;
    let data = raw["data"]
        .as_array()
        .and_then(|a| a.first())
        .ok_or_else(|| io::Error::other("Rust JSON has no data"))?;
    let totals = &data["totals"];
    let counts = |metric: &str| -> io::Result<(u64, u64)> {
        let value = &totals[metric];
        Ok((
            value["covered"]
                .as_u64()
                .ok_or_else(|| io::Error::other(format!("missing {metric}.covered")))?,
            value["count"]
                .as_u64()
                .ok_or_else(|| io::Error::other(format!("missing {metric}.count")))?,
        ))
    };
    let lines = counts("lines")?;
    let branches = counts("branches")?;
    if lines.1 == 0 || branches.1 == 0 {
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
    let revision = version("git", &["rev-parse", "HEAD"])?;
    let source_dirty = !Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(root())
        .output()?
        .stdout
        .is_empty();
    let manifest = json!({
        "component":"rust-workspace", "revision":revision, "source_dirty":source_dirty,
        "tools": {"rustc":version("rustc", &["+nightly", "--version"])?,
            "cargo":version("cargo", &["+nightly", "--version"])?,
            "collector":version("cargo", &["llvm-cov", "--version"])?},
        "test":{"status":"passed", "command":"cargo +nightly llvm-cov nextest --workspace --locked --branch --html --exclude xtask --profile ci",
            "exit_code":0,"log":"rust/test.log"},
        "lines":{"covered":lines.0,"total":lines.1},
        "branches":{"covered":branches.0,"total":branches.1},
        "report":"rust/index.html", "unavailable":[]
    });
    fs::write(
        output.join("rust.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    write_rust_crates(output, files, &manifest)?;
    Ok(())
}

fn summarize_woocommerce(output: &Path) -> io::Result<()> {
    let dir = output.join("woocommerce");
    let summary: Value = serde_json::from_slice(&fs::read(dir.join("summary.json"))?)?;
    let counts = |name: &str| -> io::Result<(u64, u64)> {
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
        Ok((covered, total))
    };
    let lines = counts("lines")?;
    let branches = counts("branches")?;
    let _paths = counts("paths")?;
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
    let revisions = version("git", &["rev-parse", "HEAD"])?;
    let source_dirty = !Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(root())
        .output()?
        .stdout
        .is_empty();
    let versions = &summary["versions"];
    let php = versions["php"]
        .as_str()
        .ok_or_else(|| io::Error::other("missing PHP version"))?;
    let phpunit = versions["phpunit"]
        .as_str()
        .ok_or_else(|| io::Error::other("missing PHPUnit version"))?;
    let xdebug = versions["xdebug"]
        .as_str()
        .ok_or_else(|| io::Error::other("missing Xdebug version"))?;
    let manifest = json!({
        "component":"woocommerce", "revision":revisions, "source_dirty":source_dirty,
        "tools":{"rustc":version("rustc", &["--version"])?, "cargo":version("cargo", &["--version"])?,
            "collector":format!("PHPUnit {phpunit} / Xdebug {xdebug}"), "php":php},
        "test":{"status":"passed", "command":"vendor/bin/phpunit --configuration phpunit.coverage.xml --path-coverage",
            "exit_code":0, "log":"woocommerce/test.log"},
        "lines":{"covered":lines.0,"total":lines.1},
        "branches":{"covered":branches.0,"total":branches.1},
        "report":"woocommerce/index.html", "unavailable":[]
    });
    fs::write(
        output.join("woocommerce.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(())
}

fn source_files(dir: &Path, found: &mut BTreeSet<PathBuf>) -> io::Result<()> {
    for item in fs::read_dir(dir)? {
        let path = item?.path();
        if path.is_dir() {
            source_files(&path, found)?;
        } else if path.extension().is_some_and(|e| e == "rs")
            && path.file_name().is_some_and(|n| n != "tests.rs")
            && !path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("_tests.rs")
        {
            found.insert(path.canonicalize()?);
        }
    }
    Ok(())
}

fn metric(file: &Value, name: &str) -> io::Result<(u64, u64)> {
    let summary = &file["summary"][name];
    Ok((
        summary["covered"]
            .as_u64()
            .ok_or_else(|| io::Error::other(format!("missing {name}.covered")))?,
        summary["count"]
            .as_u64()
            .ok_or_else(|| io::Error::other(format!("missing {name}.count")))?,
    ))
}

fn write_rust_crates(output: &Path, files: &[Value], workspace: &Value) -> io::Result<()> {
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
    let mut crates = BTreeMap::<String, PathBuf>::new();
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
        crates.insert(name.to_owned(), manifest.parent().unwrap().canonicalize()?);
    }
    if crates.is_empty() {
        return Err(io::Error::other("no product workspace crates"));
    }
    let pages = output.join("rust/crates");
    fs::create_dir_all(&pages)?;
    let mut index = String::from("<!doctype html><meta charset=\"utf-8\"><title>Rust crates</title><h1>Rust coverage by crate</h1><table border=\"1\"><tr><th>Crate</th><th>Lines</th><th>Branches</th><th>Source files</th></tr>");
    let mut summaries = Vec::new();
    let mut mapped = 0usize;
    for (name, dir) in &crates {
        let src = dir.join("src");
        let mut all_source = BTreeSet::new();
        if src.exists() {
            source_files(&src, &mut all_source)?;
        }
        let mut measured = BTreeSet::new();
        let mut lines = (0u64, 0u64);
        let mut branches = (0u64, 0u64);
        let mut page = format!("<!doctype html><meta charset=\"utf-8\"><title>{name} Rust coverage</title><h1>{name}</h1><p><a href=\"index.html\">All crates</a> · <a href=\"../index.html\">LLVM report</a></p><table border=\"1\"><tr><th>Source</th><th>Lines</th><th>Branches</th></tr>");
        for file in files {
            let Some(filename) = file["filename"].as_str() else {
                continue;
            };
            let path = Path::new(filename);
            if !path.starts_with(&src) {
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            mapped += 1;
            measured.insert(path.to_path_buf());
            let l = metric(file, "lines")?;
            let b = metric(file, "branches")?;
            lines.0 += l.0;
            lines.1 += l.1;
            branches.0 += b.0;
            branches.1 += b.1;
            let native = format!("coverage/{}.html", filename.trim_start_matches('/'));
            if !output.join("rust").join(&native).is_file() {
                return Err(io::Error::other(format!(
                    "missing annotated source: {native}"
                )));
            }
            let relative = path.strip_prefix(dir).unwrap().display().to_string();
            page.push_str(&format!(
                "<tr><td><a href=\"../{}\">{}</a></td><td>{}/{}</td><td>{}/{}</td></tr>",
                escape_html(&native),
                escape_html(&relative),
                l.0,
                l.1,
                b.0,
                b.1
            ));
        }
        for missing in all_source.difference(&measured) {
            let relative = missing.strip_prefix(dir).unwrap().display().to_string();
            page.push_str(&format!("<tr><td>{}</td><td colspan=\"2\">unavailable: no executable code in this profile or feature gated</td></tr>", escape_html(&relative)));
        }
        page.push_str("</table>");
        fs::write(pages.join(format!("{name}.html")), page)?;
        index.push_str(&format!("<tr><td><a href=\"{name}.html\">{name}</a></td><td>{}/{}</td><td>{}/{}</td><td>{} measured, {} unavailable</td></tr>",
            lines.0, lines.1, branches.0, branches.1, measured.len(), all_source.difference(&measured).count()));
        summaries.push(json!({"component":name,"lines":{"covered":lines.0,"total":lines.1},
            "branches":{"covered":branches.0,"total":branches.1},
            "report":format!("rust/crates/{name}.html"),
            "measured_files":measured.len(),
            "unavailable_files":all_source.difference(&measured).map(|p| p.strip_prefix(root()).unwrap().display().to_string()).collect::<Vec<_>>() }));
    }
    if mapped != files.len() {
        return Err(io::Error::other(format!(
            "{} Rust files did not map to a workspace crate",
            files.len() - mapped
        )));
    }
    let sum = |name: &str, key: &str| -> u64 {
        summaries.iter().filter_map(|s| s[name][key].as_u64()).sum()
    };
    for name in ["lines", "branches"] {
        for key in ["covered", "total"] {
            if sum(name, key) != workspace[name][key].as_u64().unwrap_or(0) {
                return Err(io::Error::other(format!(
                    "crate {name}.{key} totals differ from LLVM workspace totals"
                )));
            }
        }
    }
    index.push_str("</table><p>Unavailable files have no executable code in this build or require a feature not enabled by the default test run.</p>");
    fs::write(pages.join("index.html"), index)?;
    fs::write(
        output.join("rust-crates.json"),
        serde_json::to_vec_pretty(&summaries)?,
    )?;
    Ok(())
}

fn render_index(output: &Path, results: &[Value]) -> io::Result<()> {
    let mut page = String::from("<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>Monokulo coverage</title><style>body{font:16px/1.5 system-ui,sans-serif;max-width:1050px;margin:2rem auto;padding:0 1rem;color:#17212b}table{border-collapse:collapse;width:100%}th,td{border-bottom:1px solid #ccd3db;text-align:left;padding:.65rem}code{overflow-wrap:anywhere}a{color:#164e8a}.bad{color:#a32}.good{color:#175c38}small{color:#52606d}</style><h1>Monokulo coverage</h1><p>Coverage from separate Rust, browser, and WooCommerce test runs. Counts use executable lines and branches in authored product source.</p><table><thead><tr><th>Component</th><th>Tests</th><th>Lines</th><th>Branches</th><th>Reports</th></tr></thead><tbody>");
    for result in results {
        let name = result["component"].as_str().unwrap_or("unknown");
        let status = result["status"].as_str().unwrap_or("unavailable");
        let manifest_path = output.join(format!("{name}.json"));
        let manifest: Option<Value> = if manifest_path.is_file() {
            Some(serde_json::from_slice(&fs::read(&manifest_path)?)?)
        } else {
            None
        };
        let count = |metric: &str| -> String {
            manifest
                .as_ref()
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
            .as_ref()
            .and_then(|m| m["report"].as_str())
            .filter(|p| output.join(p).is_file());
        let link = if let Some(path) = report {
            format!("<a href=\"{}\">Annotated source</a>", escape_html(path))
        } else {
            "unavailable".into()
        };
        page.push_str(&format!("<tr><td>{}</td><td class=\"{}\">{} <a href=\"{}/test.log\">log</a></td><td>{}</td><td>{}</td><td>{}</td></tr>",
            escape_html(name), if status == "passed" {"good"} else {"bad"}, escape_html(status),
            escape_html(name), count("lines"), count("branches"), link));
    }
    page.push_str("</tbody></table>");
    if output.join("stress/index.html").is_file() {
        page.push_str("<p><a href=\"stress/index.html\">Engine stress report</a></p>");
    }
    if output.join("rust/crates/index.html").is_file() {
        page.push_str(
            "<p><a href=\"rust/crates/index.html\">Rust coverage by crate and source file</a></p>",
        );
    }
    if output.join("screenshots/index.html").is_file() {
        page.push_str("<p><a href=\"screenshots/index.html\">Screenshot gallery</a></p>");
        let entries: Value =
            serde_json::from_slice(&fs::read(output.join("screenshots/manifest.json"))?)?;
        let entries = entries
            .as_array()
            .ok_or_else(|| io::Error::other("screenshot manifest is not an array"))?;
        page.push_str("<h2>UI evidence</h2><div style=\"display:flex;flex-wrap:wrap;gap:1rem\">");
        // One picture per page or feature, in the gallery's order, at
        // desktop size where there is one.
        let mut groups: Vec<&str> = Vec::new();
        for group in entries.iter().filter_map(|e| e["group"].as_str()) {
            if !groups.contains(&group) {
                groups.push(group);
            }
        }
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
                let target = output.join("screenshots").join(image);
                if !target.is_file() {
                    return Err(io::Error::other(format!("missing screenshot: {image}")));
                }
                let source = format!("screenshots/{image}");
                page.push_str(&format!("<a href=\"{}\" style=\"width:30%;min-width:220px\"><img src=\"{}\" alt=\"{}\" style=\"width:100%;height:160px;object-fit:contain;background:#eee\"><br>{}</a>",
                    escape_html(&source), escape_html(&source), escape_html(group),
                    escape_html(entry["stage"].as_str().unwrap_or(group))));
            }
        }
        page.push_str("</div>");
    }
    if let Some(first) = results.iter().find_map(|r| {
        let p = output.join(format!("{}.json", r["component"].as_str()?));
        let data: Value = serde_json::from_slice(&fs::read(p).ok()?).ok()?;
        Some(data)
    }) {
        page.push_str(&format!(
            "<p><small>Revision: <code>{}</code>{}</small></p>",
            escape_html(first["revision"].as_str().unwrap_or("unknown")),
            if first["source_dirty"] == true {
                " · source tree dirty during collection"
            } else {
                ""
            }
        ));
    }
    page.push_str("<h2>Toolchains</h2><ul>");
    for result in results {
        let name = result["component"].as_str().unwrap_or("unknown");
        let manifest_path = output.join(format!("{name}.json"));
        if !manifest_path.is_file() {
            continue;
        }
        let m: Value = serde_json::from_slice(&fs::read(manifest_path)?)?;
        let tools = &m["tools"];
        page.push_str(&format!(
            "<li>{}: <code>{}</code>; <code>{}</code>; <code>{}</code></li>",
            escape_html(name),
            escape_html(tools["rustc"].as_str().unwrap_or("?")),
            escape_html(tools["cargo"].as_str().unwrap_or("?")),
            escape_html(tools["collector"].as_str().unwrap_or("?"))
        ));
    }
    page.push_str("</ul></html>");
    fs::write(output.join("index.html"), page)
}

pub(crate) fn coverage(command: &str) -> io::Result<bool> {
    let output = root().join("target/coverage");
    if command == "open" {
        let index = output.join("index.html");
        if !index.is_file() {
            eprintln!("missing report: {}", index.display());
            return Ok(false);
        }
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let status = Command::new(opener).arg(&index).status();
        return match status {
            Ok(status) => Ok(status.success()),
            Err(e) => {
                eprintln!("missing opener {opener}: {e}");
                Ok(false)
            }
        };
    }
    fs::create_dir_all(&output)?;
    let names: &[&str] = match command {
        "rust" => &["rust"],
        "browser" => &["browser"],
        "woocommerce" => &["woocommerce"],
        "stagenet" => &["stagenet"],
        "all" => &ALL,
        "report" => return report(&output),
        "summary" => {
            print!("{}", crate::summary::coverage_summary(&output)?);
            return Ok(true);
        }
        "validate" => return validate(),
        _ => {
            crate::help();
            return Ok(false);
        }
    };
    let revision = version("git", &["rev-parse", "HEAD"])?;
    write_run(&output, &revision, &[])?;
    // The collectors share nothing (each has its own target directory,
    // processes and ports), so they run side by side, each logging to its
    // own test.log.
    let (done, finished) = mpsc::channel();
    let results = thread::scope(|scope| -> io::Result<Vec<Value>> {
        for name in names {
            let (done, output) = (done.clone(), &output);
            scope.spawn(move || done.send(run(name, output)));
        }
        drop(done);
        let mut results = Vec::new();
        for result in finished {
            results.push(result?);
            results.sort_by_key(|r| names.iter().position(|n| r["component"] == *n));
            // Write after every collector so an interrupted run retains progress.
            write_run(&output, &revision, &results)?;
        }
        Ok(results)
    })?;
    let passed = results.iter().all(|r| r["status"] == "passed");
    if passed && command == "all" {
        return validate();
    }
    Ok(passed)
}

/// Writes `run.json` for `results` and renders the index from it.
fn write_run(output: &Path, revision: &str, results: &[Value]) -> io::Result<()> {
    let manifest = json!({"revision":revision,"components":results});
    fs::write(
        output.join("run.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    render_index(output, results)
}

/// `coverage report`: the index and validation for collectors that ran
/// separately. CI runs each in a job of its own and gathers their outputs
/// into one `target/coverage`; a collector that left no result is
/// unavailable, and the run fails.
fn report(output: &Path) -> io::Result<bool> {
    let mut results = Vec::new();
    for name in ALL {
        let path = output.join(name).join("result.json");
        results.push(if path.is_file() {
            serde_json::from_slice(&fs::read(path)?)?
        } else {
            json!({"component":name,"status":"unavailable","exit_code":null,"log":format!("{name}/test.log"),
                "reason":"no result: the collector did not run or did not finish"})
        });
    }
    // The collectors' own revision (validation rejects a mix), or this
    // checkout's when none recorded one.
    let revision = ALL.iter().find_map(|name| {
        let manifest: Value =
            serde_json::from_slice(&fs::read(output.join(format!("{name}.json"))).ok()?).ok()?;
        manifest["revision"].as_str().map(str::to_owned)
    });
    let revision = match revision {
        Some(r) => r,
        None => version("git", &["rev-parse", "HEAD"])?,
    };
    write_run(output, &revision, &results)?;
    for result in &results {
        eprintln!(
            "coverage {}: {}",
            result["component"].as_str().unwrap_or("?"),
            result["status"].as_str().unwrap_or("?")
        );
    }
    if results.iter().all(|r| r["status"] == "passed") {
        validate()
    } else {
        Ok(false)
    }
}

fn validate() -> io::Result<bool> {
    match crate::validate::validate(&root(), &root().join("target/coverage")) {
        Ok(()) => {
            println!("coverage artifact validation passed");
            Ok(true)
        }
        Err(problem) => {
            eprintln!("coverage artifact validation failed: {problem}");
            Ok(false)
        }
    }
}
