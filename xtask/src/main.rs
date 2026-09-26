use serde_json::{json, Value};
use std::{env, fs, io, path::{Path, PathBuf}, process::{Command, ExitCode, Stdio}};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn help() {
    println!("Usage: cargo xtask coverage <rust|browser|woocommerce|all|open>\n\n\
        rust          Refresh nightly and cargo-llvm-cov; run workspace tests and collect Rust coverage\n\
        browser       Run deterministic Playwright tests and collect authored browser source coverage\n\
        woocommerce   Run default PHPUnit tests in wp-env and collect plugin coverage\n\
        all           Run every collector; preserve successful reports if another fails\n\
        open          Open target/coverage/index.html in the default browser\n\
        --help        Show this help");
}

fn run(component: &str, output: &Path) -> io::Result<Value> {
    let script = root().join("scripts").join(format!("coverage-{component}.sh"));
    let dir = output.join(component);
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
    eprintln!("coverage {component}: {} (log: {})", if status.success() { "passed" } else { "failed" }, log_path.display());
    Ok(json!({"component":component,"status":if status.success() {"passed"} else {"failed"},
        "exit_code":code,"log":format!("{component}/test.log")}))
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
        "all" => &["rust", "browser", "woocommerce"],
        _ => { help(); return Ok(false); }
    };
    let revision = Command::new("git").args(["rev-parse", "HEAD"])
        .current_dir(root()).output()?;
    let revision = String::from_utf8_lossy(&revision.stdout).trim().to_owned();
    let mut results = Vec::new();
    for name in names {
        results.push(run(name, &output)?);
        // Write after every collector so an interrupted run retains progress.
        let manifest = json!({"revision":revision,"components":results});
        fs::write(output.join("run.json"), serde_json::to_vec_pretty(&manifest)?)?;
    }
    Ok(results.iter().all(|r| r["status"] == "passed"))
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
