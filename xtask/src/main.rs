use serde_json::{json, Value};
use std::{collections::{BTreeMap, BTreeSet}, env, fs, io, path::{Path, PathBuf}, process::{Command, ExitCode, Stdio}};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn help() {
    println!("Usage: cargo xtask coverage <rust|browser|woocommerce|stagenet|all|open>\n\n\
        rust          Refresh nightly and cargo-llvm-cov; run workspace tests and collect Rust coverage\n\
        browser       Run deterministic Playwright tests and collect authored browser source coverage\n\
        woocommerce   Run default PHPUnit tests in wp-env and collect plugin coverage\n\
        stagenet      Explicit extended run: paid browser tests and separate instrumented report\n\
        all           Run every collector; preserve successful reports if another fails\n\
        open          Open target/coverage/index.html in the default browser\n\
        --help        Show this help");
}

fn run(component: &str, output: &Path) -> io::Result<Value> {
    let script = root().join("scripts").join(format!("coverage-{component}.sh"));
    let dir = output.join(component);
    let manifest_path = output.join(format!("{component}.json"));
    if manifest_path.exists() { fs::remove_file(&manifest_path)?; }
    if dir.exists() { fs::remove_dir_all(&dir)?; }
    fs::create_dir_all(&dir)?;
    let log_path = dir.join("test.log");
    let log = fs::File::create(&log_path)?;
    let status = if script.exists() {
        eprintln!("coverage {component}: running {}", script.display());
        Command::new("bash")
            .arg(&script)
            .current_dir(root())
            .env("COVERAGE_OUTPUT", &dir)
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .status()?
    } else {
        eprintln!("coverage {component}: missing collector script {}", script.display());
        return Ok(json!({"component":component,"status":"unavailable","exit_code":null,
            "log":format!("{component}/test.log"),"reason":format!("missing collector script {}", script.display())}));
    };
    let code = status.code().unwrap_or(1);
    let mut passed = status.success();
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
        for required in ["stagenet.json", "stagenet/index.html", "stagenet/screenshots/index.html"] {
            if !output.join(required).is_file() {
                eprintln!("coverage stagenet: missing report {required}");
                passed = false;
            }
        }
    }
    eprintln!("coverage {component}: {} (log: {})", if passed { "passed" } else { "failed" }, log_path.display());
    Ok(json!({"component":component,"status":if passed {"passed"} else {"failed"},
        "exit_code":code,"log":format!("{component}/test.log")}))
}

fn version(command: &str, args: &[&str]) -> io::Result<String> {
    let output = Command::new(command).args(args).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!("{command} {} failed", args.join(" "))));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn summarize_rust(output: &Path) -> io::Result<()> {
    let raw: Value = serde_json::from_slice(&fs::read(output.join("rust/raw.json"))?)?;
    let data = raw["data"].as_array().and_then(|a| a.first())
        .ok_or_else(|| io::Error::other("Rust JSON has no data"))?;
    let totals = &data["totals"];
    let counts = |metric: &str| -> io::Result<(u64, u64)> {
        let value = &totals[metric];
        Ok((value["covered"].as_u64().ok_or_else(|| io::Error::other(format!("missing {metric}.covered")))?,
            value["count"].as_u64().ok_or_else(|| io::Error::other(format!("missing {metric}.count")))?))
    };
    let lines = counts("lines")?;
    let branches = counts("branches")?;
    if lines.1 == 0 || branches.1 == 0 {
        return Err(io::Error::other("Rust line or branch denominator is zero"));
    }
    let files = data["files"].as_array().ok_or_else(|| io::Error::other("Rust JSON has no files"))?;
    for crate_name in ["scanner", "monokulo"] {
        let branch_count: u64 = files.iter().filter(|f| {
            f["filename"].as_str().is_some_and(|s| s.contains(&format!("/crates/{crate_name}/src/")))
        }).filter_map(|f| f["summary"]["branches"]["count"].as_u64()).sum();
        if branch_count == 0 { return Err(io::Error::other(format!("missing branch data for {crate_name}"))); }
    }
    let revision = version("git", &["rev-parse", "HEAD"])?;
    let source_dirty = !Command::new("git").args(["status", "--porcelain"])
        .current_dir(root()).output()?.stdout.is_empty();
    let manifest = json!({
        "component":"rust-workspace", "revision":revision, "source_dirty":source_dirty,
        "tools": {"rustc":version("rustc", &["+nightly", "--version"])?,
            "cargo":version("cargo", &["+nightly", "--version"])?,
            "collector":version("cargo", &["llvm-cov", "--version"])?},
        "test":{"status":"passed", "command":"cargo +nightly llvm-cov --workspace --locked --branch --html --exclude xtask",
            "exit_code":0,"log":"rust/test.log"},
        "lines":{"covered":lines.0,"total":lines.1},
        "branches":{"covered":branches.0,"total":branches.1},
        "report":"rust/index.html", "unavailable":[]
    });
    fs::write(output.join("rust.json"), serde_json::to_vec_pretty(&manifest)?)?;
    write_rust_crates(output, files, &manifest)?;
    Ok(())
}

fn summarize_woocommerce(output: &Path) -> io::Result<()> {
    let dir = output.join("woocommerce");
    let summary: Value = serde_json::from_slice(&fs::read(dir.join("summary.json"))?)?;
    let counts = |name: &str| -> io::Result<(u64, u64)> {
        let value = &summary[name];
        let covered = value["covered"].as_u64().ok_or_else(|| io::Error::other(format!("missing PHP {name}.covered")))?;
        let total = value["total"].as_u64().ok_or_else(|| io::Error::other(format!("missing PHP {name}.total")))?;
        if total == 0 || covered > total { return Err(io::Error::other(format!("invalid PHP {name} counts"))); }
        Ok((covered, total))
    };
    let lines = counts("lines")?;
    let branches = counts("branches")?;
    let _paths = counts("paths")?;
    let files = summary["files"].as_array().ok_or_else(|| io::Error::other("missing PHP file list"))?;
    let expected = ["monokulo.php", "class-wc-gateway-monokulo.php"];
    if files.len() != expected.len() || expected.iter().any(|name| {
        !files.iter().any(|f| f["name"].as_str() == Some(name))
    }) { return Err(io::Error::other("PHP coverage contains missing or non-plugin source files")); }
    for required in ["index.html", "xml/index.xml", "clover.xml", "junit.xml",
        "includes/class-wc-gateway-monokulo.php_branch.html"] {
        if !dir.join(required).is_file() { return Err(io::Error::other(format!("missing PHP report {required}"))); }
    }
    let revisions = version("git", &["rev-parse", "HEAD"])?;
    let source_dirty = !Command::new("git").args(["status", "--porcelain"])
        .current_dir(root()).output()?.stdout.is_empty();
    let versions = &summary["versions"];
    let php = versions["php"].as_str().ok_or_else(|| io::Error::other("missing PHP version"))?;
    let phpunit = versions["phpunit"].as_str().ok_or_else(|| io::Error::other("missing PHPUnit version"))?;
    let xdebug = versions["xdebug"].as_str().ok_or_else(|| io::Error::other("missing Xdebug version"))?;
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
    fs::write(output.join("woocommerce.json"), serde_json::to_vec_pretty(&manifest)?)?;
    Ok(())
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
        .replace('"', "&quot;").replace('\'', "&#39;")
}

fn source_files(dir: &Path, found: &mut BTreeSet<PathBuf>) -> io::Result<()> {
    for item in fs::read_dir(dir)? {
        let path = item?.path();
        if path.is_dir() { source_files(&path, found)?; }
        else if path.extension().is_some_and(|e| e == "rs")
            && path.file_name().is_some_and(|n| n != "tests.rs")
            && !path.file_name().unwrap().to_string_lossy().ends_with("_tests.rs") {
            found.insert(path.canonicalize()?);
        }
    }
    Ok(())
}

fn metric(file: &Value, name: &str) -> io::Result<(u64, u64)> {
    let summary = &file["summary"][name];
    Ok((summary["covered"].as_u64().ok_or_else(|| io::Error::other(format!("missing {name}.covered")))?,
        summary["count"].as_u64().ok_or_else(|| io::Error::other(format!("missing {name}.count")))?))
}

fn write_rust_crates(output: &Path, files: &[Value], workspace: &Value) -> io::Result<()> {
    let metadata: Value = serde_json::from_slice(&Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1", "--locked"])
        .current_dir(root()).output()?.stdout)?;
    let members: BTreeSet<&str> = metadata["workspace_members"].as_array()
        .ok_or_else(|| io::Error::other("Cargo metadata has no workspace members"))?
        .iter().filter_map(Value::as_str).collect();
    let mut crates = BTreeMap::<String, PathBuf>::new();
    for package in metadata["packages"].as_array().ok_or_else(|| io::Error::other("Cargo metadata has no packages"))? {
        if !package["id"].as_str().is_some_and(|id| members.contains(id)) { continue; }
        let name = package["name"].as_str().ok_or_else(|| io::Error::other("package without name"))?;
        if name == "xtask" { continue; }
        let manifest = Path::new(package["manifest_path"].as_str()
            .ok_or_else(|| io::Error::other("package without manifest path"))?);
        crates.insert(name.to_owned(), manifest.parent().unwrap().canonicalize()?);
    }
    if crates.is_empty() { return Err(io::Error::other("no product workspace crates")); }
    let pages = output.join("rust/crates");
    fs::create_dir_all(&pages)?;
    let mut index = String::from("<!doctype html><meta charset=\"utf-8\"><title>Rust crates</title><h1>Rust coverage by crate</h1><table border=\"1\"><tr><th>Crate</th><th>Lines</th><th>Branches</th><th>Source files</th></tr>");
    let mut summaries = Vec::new();
    let mut mapped = 0usize;
    for (name, dir) in &crates {
        let src = dir.join("src");
        let mut all_source = BTreeSet::new();
        if src.exists() { source_files(&src, &mut all_source)?; }
        let mut measured = BTreeSet::new();
        let mut lines = (0u64, 0u64);
        let mut branches = (0u64, 0u64);
        let mut page = format!("<!doctype html><meta charset=\"utf-8\"><title>{name} Rust coverage</title><h1>{name}</h1><p><a href=\"index.html\">All crates</a> · <a href=\"../index.html\">LLVM report</a></p><table border=\"1\"><tr><th>Source</th><th>Lines</th><th>Branches</th></tr>");
        for file in files {
            let Some(filename) = file["filename"].as_str() else { continue; };
            let path = Path::new(filename);
            if !path.starts_with(&src) { continue; }
            if !path.extension().is_some_and(|e| e == "rs") { continue; }
            mapped += 1;
            measured.insert(path.to_path_buf());
            let l = metric(file, "lines")?;
            let b = metric(file, "branches")?;
            lines.0 += l.0; lines.1 += l.1;
            branches.0 += b.0; branches.1 += b.1;
            let native = format!("coverage/{}.html", filename.trim_start_matches('/'));
            if !output.join("rust").join(&native).is_file() {
                return Err(io::Error::other(format!("missing annotated source: {native}")));
            }
            let relative = path.strip_prefix(dir).unwrap().display().to_string();
            page.push_str(&format!("<tr><td><a href=\"../{}\">{}</a></td><td>{}/{}</td><td>{}/{}</td></tr>",
                escape_html(&native), escape_html(&relative), l.0, l.1, b.0, b.1));
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
            "unavailable_files":all_source.difference(&measured).map(|p| p.strip_prefix(&root()).unwrap().display().to_string()).collect::<Vec<_>>() }));
    }
    if mapped != files.len() { return Err(io::Error::other(format!("{} Rust files did not map to a workspace crate", files.len() - mapped))); }
    let sum = |name: &str, key: &str| -> u64 { summaries.iter().filter_map(|s| s[name][key].as_u64()).sum() };
    for name in ["lines", "branches"] {
        for key in ["covered", "total"] {
            if sum(name, key) != workspace[name][key].as_u64().unwrap_or(0) {
                return Err(io::Error::other(format!("crate {name}.{key} totals differ from LLVM workspace totals")));
            }
        }
    }
    index.push_str("</table><p>Unavailable files have no executable code in this build or require a feature not enabled by the default test run.</p>");
    fs::write(pages.join("index.html"), index)?;
    fs::write(output.join("rust-crates.json"), serde_json::to_vec_pretty(&summaries)?)?;
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
        } else { None };
        let count = |metric: &str| -> String {
            manifest.as_ref().and_then(|m| {
                Some(format!("{} / {}", m[metric]["covered"].as_u64()?, m[metric]["total"].as_u64()?))
            }).unwrap_or_else(|| "unavailable".into())
        };
        let report = manifest.as_ref().and_then(|m| m["report"].as_str())
            .filter(|p| output.join(p).is_file());
        let link = if let Some(path) = report {
            format!("<a href=\"{}\">Annotated source</a>", escape_html(path))
        } else { "unavailable".into() };
        page.push_str(&format!("<tr><td>{}</td><td class=\"{}\">{} <a href=\"{}/test.log\">log</a></td><td>{}</td><td>{}</td><td>{}</td></tr>",
            escape_html(name), if status == "passed" {"good"} else {"bad"}, escape_html(status),
            escape_html(name), count("lines"), count("branches"), link));
    }
    page.push_str("</tbody></table>");
    if output.join("rust/crates/index.html").is_file() {
        page.push_str("<p><a href=\"rust/crates/index.html\">Rust coverage by crate and source file</a></p>");
    }
    if output.join("screenshots/index.html").is_file() {
        page.push_str("<p><a href=\"screenshots/index.html\">Screenshot gallery</a></p>");
        let entries: Value = serde_json::from_slice(&fs::read(output.join("screenshots/manifest.json"))?)?;
        let entries = entries.as_array().ok_or_else(|| io::Error::other("screenshot manifest is not an array"))?;
        page.push_str("<h2>UI evidence</h2><div style=\"display:flex;flex-wrap:wrap;gap:1rem\">");
        for group in ["checkout", "pos", "challenge"] {
            if let Some(entry) = entries.iter().find(|e| e["group"] == group && e["stage"] != "failure") {
                let image = entry["image"].as_str().ok_or_else(|| io::Error::other("screenshot entry has no image"))?;
                let target = output.join("screenshots").join(image);
                if !target.is_file() { return Err(io::Error::other(format!("missing screenshot: {image}"))); }
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
        page.push_str(&format!("<p><small>Revision: <code>{}</code>{}</small></p>",
            escape_html(first["revision"].as_str().unwrap_or("unknown")),
            if first["source_dirty"] == true {" · source tree dirty during collection"} else {""}));
    }
    page.push_str("<h2>Toolchains</h2><ul>");
    for result in results {
        let name = result["component"].as_str().unwrap_or("unknown");
        let manifest_path = output.join(format!("{name}.json"));
        if !manifest_path.is_file() { continue; }
        let m: Value = serde_json::from_slice(&fs::read(manifest_path)?)?;
        let tools = &m["tools"];
        page.push_str(&format!("<li>{}: <code>{}</code>; <code>{}</code>; <code>{}</code></li>",
            escape_html(name), escape_html(tools["rustc"].as_str().unwrap_or("?")),
            escape_html(tools["cargo"].as_str().unwrap_or("?")),
            escape_html(tools["collector"].as_str().unwrap_or("?"))));
    }
    page.push_str("</ul></html>");
    fs::write(output.join("index.html"), page)
}

fn coverage(command: &str) -> io::Result<bool> {
    let output = root().join("target/coverage");
    if command == "open" {
        let index = output.join("index.html");
        if !index.is_file() { eprintln!("missing report: {}", index.display()); return Ok(false); }
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        let status = Command::new(opener).arg(&index).status();
        return match status {
            Ok(status) => Ok(status.success()),
            Err(e) => { eprintln!("missing opener {opener}: {e}"); Ok(false) }
        };
    }
    fs::create_dir_all(&output)?;
    let names: &[&str] = match command {
        "rust" => &["rust"],
        "browser" => &["browser"],
        "woocommerce" => &["woocommerce"],
        "stagenet" => &["stagenet"],
        "all" => &["rust", "browser", "woocommerce"],
        _ => { help(); return Ok(false); }
    };
    let revision = Command::new("git").args(["rev-parse", "HEAD"])
        .current_dir(root()).output()?;
    let revision = String::from_utf8_lossy(&revision.stdout).trim().to_owned();
    let mut results = Vec::new();
    fs::write(output.join("run.json"), serde_json::to_vec_pretty(&json!({
        "revision":revision,"components":results
    }))?)?;
    for name in names {
        results.push(run(name, &output)?);
        // Write after every collector so an interrupted run retains progress.
        let manifest = json!({"revision":revision,"components":results});
        fs::write(output.join("run.json"), serde_json::to_vec_pretty(&manifest)?)?;
        render_index(&output, manifest["components"].as_array().unwrap())?;
    }
    let passed = results.iter().all(|r| r["status"] == "passed");
    if passed && command == "all" {
        let validation = Command::new("python3").arg(root().join("scripts/validate-coverage.py"))
            .current_dir(root()).status()
            .map_err(|e| io::Error::other(format!("missing prerequisite: python3 ({e})")))?;
        if !validation.success() { return Ok(false); }
    }
    Ok(passed)
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() != 2 || args.iter().any(|a| a == "--help" || a == "-h") {
        help();
        return if args.iter().any(|a| a == "--help" || a == "-h") { ExitCode::SUCCESS } else { ExitCode::FAILURE };
    }
    if args[0] != "coverage" { help(); return ExitCode::FAILURE; }
    match coverage(&args[1]) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => { eprintln!("coverage: {e}"); ExitCode::FAILURE }
    }
}
