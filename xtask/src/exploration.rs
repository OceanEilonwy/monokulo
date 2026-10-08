//! Engine verification runs: `cargo xtask engine fuzz` runs one
//! coverage-guided fuzz campaign and records its replay settings and measured
//! exploration; `engine properties` and `engine scale` run the property and
//! scale tests, the property run reporting the scenarios it reached. None of
//! them infers line coverage: they record what the fuzzer and the tests
//! report (docs/ENGINE_VERIFICATION.md).

use regex::Regex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::LazyLock,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const TERMINAL_STATES: [&str; 4] = [
    "passed",
    "failed",
    "invalid-evidence",
    "insufficient-exploration",
];

/// Each target's budget, per-input deadline and input size limit: IO
/// histories need a larger deadline and sustained budget than policies.
fn limits(target: &str) -> Option<(u64, u64, u64)> {
    Some(match target {
        "history" => (900, 60, 4096),
        "portfolio" => (900, 120, 260),
        "notifications" | "queue" => (600, 60, 4096),
        "scheduler" | "resources" | "inputs" | "mempool" | "status" => (300, 10, 4096),
        _ => return None,
    })
}

pub(crate) const HELP: &str = "\
        engine fuzz TARGET [SECONDS] [zmq]\n\
                      One fuzz campaign (scheduler, resources, inputs, queue, mempool, status, history,\n\
                      notifications or portfolio): reviewed seeds copied into the corpus, a calibrated\n\
                      build, then libFuzzer for SECONDS, with its evidence under\n\
                      target/engine-exploration/fuzz (ENGINE_FUZZ_SEED sets the seed; needs cargo-fuzz)\n\
        engine properties [default|zmq]\n\
                      The engine's and key custody's property tests (PROPTEST_CASES, ENGINE_PROOF_CASES and\n\
                      PROPTEST_RNG_SEED as set), then target/engine-exploration/properties/report.json\n\
        engine scale [default|zmq]\n\
                      The scale tests: generated cases and every large fixture, one test at a time\n\
                      (target/engine-scale)";

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// Written whole or not at all, so an interrupted run never leaves half a report.
fn write_json(path: &Path, data: &Value) -> io::Result<()> {
    let pending = path.with_extension("json.tmp");
    fs::write(&pending, serde_json::to_string_pretty(data)? + "\n")?;
    fs::rename(pending, path)
}

fn read_json(path: &Path) -> io::Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

/// The reviewed seeds of a target, which must exist.
fn seed_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    if !dir.is_dir() {
        return Err(format!(
            "Reviewed seed directory is missing: {}",
            dir.display()
        ));
    }
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(format!(
            "Reviewed seed directory is empty: {}",
            dir.display()
        ));
    }
    Ok(files)
}

/// Copies the reviewed seeds into the corpus under their content's hash: a
/// reviewed input is identified by content, not its possibly cached name, and
/// the copy is refreshed even if an earlier campaign changed it.
pub(crate) fn sync_seeds(seeds: &Path, corpus: &Path) -> Result<(), String> {
    let files = seed_files(seeds)?;
    fs::create_dir_all(corpus).map_err(|e| e.to_string())?;
    for seed in files {
        let bytes = fs::read(&seed).map_err(|e| e.to_string())?;
        fs::write(corpus.join(format!("reviewed-{}", sha256(&bytes))), bytes)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The corpus's files, bytes and distinct contents.
fn corpus(dir: &Path) -> Value {
    let files: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    let mut bytes = 0u64;
    let mut hashes = BTreeSet::new();
    for file in &files {
        let content = fs::read(file).unwrap_or_default();
        bytes += content.len() as u64;
        hashes.insert(sha256(&content));
    }
    json!({"files": files.len(), "bytes": bytes, "sha256": hashes})
}

static SAMPLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"#(\d+)\s+(INITED|NEW|REDUCE|DONE).*?cov:\s*(\d+)\s+ft:\s*(\d+)").unwrap()
});
static STAT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"stat::([a-z_]+):\s*(\d+)").unwrap());

/// What libFuzzer's log says it explored: the first and last coverage
/// samples, its final stats, and the growth between them.
pub(crate) fn summarize(log: &str) -> Value {
    let samples: Vec<Value> = log
        .lines()
        .filter_map(|line| SAMPLE.captures(line))
        .map(|c| {
            json!({"executions": c[1].parse::<u64>().unwrap_or(0), "phase": &c[2],
                "coverage": c[3].parse::<u64>().unwrap_or(0), "features": c[4].parse::<u64>().unwrap_or(0)})
        })
        .collect();
    let initial = samples.iter().find(|s| s["phase"] == "INITED").cloned();
    let fin = samples.last().cloned();
    let stats: Map<String, Value> = STAT
        .captures_iter(log)
        .map(|c| (c[1].to_string(), json!(c[2].parse::<u64>().unwrap_or(0))))
        .collect();
    let delta = |key: &str| match (&initial, &fin) {
        (Some(i), Some(f)) => json!(f[key].as_i64().unwrap_or(0) - i[key].as_i64().unwrap_or(0)),
        _ => Value::Null,
    };
    json!({
        "initial": initial, "final": fin, "stats": stats,
        "executions_after_initialization": delta("executions"),
        "coverage_growth": delta("coverage"), "feature_growth": delta("features"),
    })
}

/// Whether a finished campaign's evidence counts.
pub(crate) fn evidence_status(
    exploration: &Value,
    exit_code: i64,
) -> (&'static str, Option<&'static str>) {
    if exit_code != 0 {
        return (
            "failed",
            Some("Fuzzer exited unsuccessfully; inspect the raw log."),
        );
    }
    let (initial, fin, delta) = (
        &exploration["initial"],
        &exploration["final"],
        exploration["executions_after_initialization"].as_i64(),
    );
    if initial.is_null() || fin.is_null() || fin["phase"] != "DONE" || delta.is_none_or(|d| d < 0) {
        return (
            "invalid-evidence",
            Some("Missing, malformed or inconsistent completed exploration evidence."),
        );
    }
    if delta == Some(0) {
        return (
            "insufficient-exploration",
            Some("No inputs explored after initialization; increase the budget."),
        );
    }
    ("passed", None)
}

/// The scenario observations the engine wrote while it ran (one JSON line per
/// case), added up; command names lose their arguments.
pub(crate) fn semantics(dir: &Path) -> (u64, BTreeMap<String, u64>) {
    let mut counts = BTreeMap::new();
    let mut cases = 0;
    let Ok(entries) = fs::read_dir(dir) else {
        return (0, counts);
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(name.starts_with("semantics.") && name.ends_with(".jsonl")) {
            continue;
        }
        for line in fs::read_to_string(entry.path()).unwrap_or_default().lines() {
            let Ok(Value::Object(map)) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            cases += 1;
            for (mut key, count) in map {
                if key.starts_with("command:") || key.starts_with("selected-command:") {
                    key = key.split(['(', '{', ' ']).next().unwrap_or("").to_string();
                }
                *counts.entry(key).or_insert(0) += count.as_u64().unwrap_or(0);
            }
        }
    }
    (cases, counts)
}

/// Programs and their environment, so tests can stand fakes in for git,
/// rustc and cargo.
#[derive(Clone, Default)]
pub(crate) struct Tools {
    pub(crate) vars: BTreeMap<String, String>,
}

impl Tools {
    fn command(&self, program: &str, root: &Path) -> Command {
        let mut command = Command::new(program);
        command.current_dir(root).envs(&self.vars);
        command
    }
    fn var(&self, name: &str) -> Option<String> {
        self.vars.get(name).cloned().or_else(|| env::var(name).ok())
    }
    fn output(&self, root: &Path, program: &str, args: &[&str]) -> String {
        self.command(program, root)
            .args(args)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    }
}

/// Runs a program to a log file and our own output at once (`2>&1 | tee`),
/// returning its exit code.
pub(crate) fn tee(mut command: Command, log: &Path) -> io::Result<i32> {
    let (mut reader, writer) = io::pipe()?;
    let mut child = command.stdout(writer.try_clone()?).stderr(writer).spawn()?;
    drop(command);
    let mut file = fs::File::create(log)?;
    let mut buffer = [0u8; 8192];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        file.write_all(&buffer[..n])?;
        io::stdout().write_all(&buffer[..n])?;
    }
    Ok(child.wait()?.code().unwrap_or(1))
}

/// Runs every reviewed seed once against the built fuzzer, each under a
/// watchdog, so a slow or crashing seed stops the campaign before it starts.
fn calibrate(
    output: &Path,
    seeds: &Path,
    binary: &Path,
    timeout: u64,
    tools: &Tools,
) -> Result<(), String> {
    let files = match seed_files(seeds) {
        Ok(files) => files,
        Err(problem) => {
            write_json(
                &output.join("calibration.json"),
                &json!({"status": "failed", "reason": problem, "seeds": []}),
            )
            .map_err(|e| e.to_string())?;
            return Err(problem);
        }
    };
    let mut results = Vec::new();
    for seed in &files {
        let name = seed.file_name().unwrap().to_string_lossy().into_owned();
        let log = output.join(format!("calibration-{name}.log"));
        let started = Instant::now();
        let file = fs::File::create(&log).map_err(|e| e.to_string())?;
        let code = match Command::new(binary.canonicalize().unwrap_or(binary.to_path_buf()))
            .args(["-runs=1", "-seed=1", &format!("-timeout={timeout}")])
            .arg(seed.canonicalize().unwrap_or(seed.clone()))
            .envs(&tools.vars)
            .stdout(file.try_clone().map_err(|e| e.to_string())?)
            .stderr(file)
            .spawn()
        {
            Err(e) => {
                fs::write(&log, format!("{e}\n")).ok();
                127
            }
            Ok(mut child) => {
                match wait_with_deadline(&mut child, Duration::from_secs(timeout + 10)) {
                    Some(code) => code,
                    None => {
                        fs::write(&log, "Seed exceeded calibration watchdog\n").ok();
                        124
                    }
                }
            }
        };
        results.push(
            json!({"seed": name, "seconds": started.elapsed().as_secs_f64(), "exit_code": code}),
        );
    }
    let failed = results.iter().any(|r| r["exit_code"] != 0);
    let mut durations: Vec<f64> = results
        .iter()
        .map(|r| r["seconds"].as_f64().unwrap_or(0.0))
        .collect();
    durations.sort_by(f64::total_cmp);
    let n = durations.len();
    let median = if n % 2 == 1 {
        durations[n / 2]
    } else {
        (durations[n / 2 - 1] + durations[n / 2]) / 2.0
    };
    write_json(
        &output.join("calibration.json"),
        &json!({
            "status": if failed { "failed" } else { "passed" },
            "binary_sha256": sha256(&fs::read(binary).unwrap_or_default()),
            "seeds": results, "median_seconds": median, "max_seconds": durations[n - 1], "timeout_seconds": timeout,
        }),
    )
    .map_err(|e| e.to_string())?;
    if failed {
        return Err("Seed calibration failed; inspect logs.".into());
    }
    Ok(())
}

/// Waits for `child` until `limit`, killing it after; `None` when it ran out.
pub(crate) fn wait_with_deadline(child: &mut std::process::Child, limit: Duration) -> Option<i32> {
    let deadline = Instant::now() + limit;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status.code().unwrap_or(1));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A campaign's settings: what replaying it needs.
struct Campaign {
    target: String,
    features: String,
    seconds: u64,
    timeout: u64,
    max_len: u64,
    seed: u64,
}

/// Records the replay settings in a new, empty campaign folder.
fn begin(
    root: &Path,
    output: &Path,
    corpus_dir: &Path,
    seeds: &Path,
    c: &Campaign,
    tools: &Tools,
) -> Result<(), String> {
    if fs::read_dir(output)
        .map_err(|e| e.to_string())?
        .next()
        .is_some()
    {
        return Err("Refusing to reuse a nonempty campaign directory.".into());
    }
    write_json(
        &output.join("report.json"),
        &json!({"schema": 2, "status": "running"}),
    )
    .map_err(|e| e.to_string())?;
    let rustc = tools.output(root, "rustc", &["-Vv"]);
    let host = rustc
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .unwrap_or("")
        .to_string();
    let toolchain = tools
        .var("RUSTUP_TOOLCHAIN")
        .unwrap_or_else(|| tools.output(root, "rustup", &["show", "active-toolchain"]));
    let locks: Map<String, Value> = ["Cargo.lock", "fuzz/Cargo.lock"]
        .iter()
        .map(|p| {
            (
                p.to_string(),
                json!(sha256(&fs::read(root.join(p)).unwrap_or_default())),
            )
        })
        .collect();
    let reviewed: Map<String, Value> = seed_files(seeds)?
        .iter()
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                json!(sha256(&fs::read(p).unwrap_or_default())),
            )
        })
        .collect();
    let sanitizers: Map<String, Value> = ["ASAN_OPTIONS", "LSAN_OPTIONS", "UBSAN_OPTIONS"]
        .iter()
        .map(|n| (n.to_string(), json!(tools.var(n))))
        .collect();
    write_json(
        &output.join("replay.json"),
        &json!({
            "schema": 2,
            "settings": {"operation": "begin", "target": c.target, "features": c.features, "seconds": c.seconds,
                "timeout": c.timeout, "max_len": c.max_len, "seed": c.seed},
            "revision": tools.output(root, "git", &["rev-parse", "HEAD"]),
            "dirty": !tools.output(root, "git", &["status", "--porcelain"]).is_empty(),
            "platform": format!("{}-{}", env::consts::OS, env::consts::ARCH),
            "rustc": rustc, "fuzz_target_triple": host,
            "cargo": tools.output(root, "cargo", &["-V"]), "cargo_fuzz": tools.output(root, "cargo", &["fuzz", "--version"]),
            "toolchain": toolchain, "sanitizer_environment": sanitizers, "lock_sha256": locks,
            "reviewed_seeds": reviewed, "started": now(), "corpus": corpus(corpus_dir),
        }),
    )
    .map_err(|e| e.to_string())
}

/// Marks a campaign that stopped before reporting as failed at `stage`,
/// unless it already reached a verdict.
fn incomplete(output: &Path, stage: &str, exit_code: i32) {
    let path = output.join("report.json");
    let current = read_json(&path).unwrap_or(Value::Null);
    if !TERMINAL_STATES.contains(&current["status"].as_str().unwrap_or("")) {
        let _ = write_json(
            &path,
            &json!({"schema": 2, "status": "failed", "stage": stage, "exit_code": exit_code,
                "reason": "Campaign stopped before completed exploration reporting."}),
        );
    }
}

/// The finished campaign's report: its verdict, corpus growth, measured
/// exploration and scenario observations. Err holds the verdict's reason.
pub(crate) fn finish(
    output: &Path,
    corpus_dir: &Path,
    exit_code: i32,
) -> io::Result<Result<(), String>> {
    let before = read_json(&output.join("replay.json"))?;
    let after = corpus(corpus_dir);
    let (cases, counts) = semantics(output);
    let exploration = summarize(&fs::read_to_string(output.join("fuzzer.log"))?);
    let (status, reason) = evidence_status(&exploration, exit_code.into());
    let known: BTreeSet<&str> = before["corpus"]["sha256"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let new = after["sha256"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|h| !known.contains(h))
        .count();
    let data = json!({
        "schema": 2, "status": status, "reason": reason, "exit_code": exit_code,
        "wall_seconds": now() - before["started"].as_f64().unwrap_or(0.0), "corpus": after, "new_unique_inputs": new,
        "exploration": exploration, "semantic_cases": cases, "semantic_observations": counts,
    });
    write_json(&output.join("report.json"), &data)?;
    println!("{}", serde_json::to_string_pretty(&data)?);
    Ok(if status == "passed" {
        Ok(())
    } else {
        Err(reason.unwrap_or("").to_string())
    })
}

/// A new campaign folder of its own: every run has independent evidence,
/// even when the seed repeats.
fn campaign_dir(parent: &Path, revision: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(parent)?;
    loop {
        let mut bytes = [0u8; 4];
        getrandom::fill(&mut bytes).map_err(io::Error::other)?;
        let dir = parent.join(format!("{revision}-{}", hex::encode(bytes)));
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

/// One fuzz campaign from the repository at `root`; returns the exit code
/// the campaign ends with (the fuzzer's, or the failed step's).
pub(crate) fn fuzz(root: &Path, args: &[&str], tools: &Tools) -> io::Result<i32> {
    let usage = || {
        io::Error::other(
        "Target must be scheduler, resources, inputs, queue, mempool, status, history, notifications, or portfolio.",
    )
    };
    let (target, seconds, features) = match args {
        [t] => (*t, None, ""),
        [t, s] => (*t, Some(*s), ""),
        [t, s, f] => (*t, Some(*s), *f),
        _ => return Err(usage()),
    };
    let (budget, deadline, max_len) = limits(target).ok_or_else(usage)?;
    let seconds = match seconds {
        None => budget,
        Some(s) => s
            .parse()
            .ok()
            .filter(|s| *s > 0)
            .ok_or_else(|| io::Error::other("Seconds must be a positive integer."))?,
    };
    if !matches!(features, "" | "zmq") {
        return Err(io::Error::other("Features must be empty or zmq."));
    }
    // cargo-fuzz passes -seed through to libFuzzer. Every run prints it for replay.
    let seed: u64 = tools
        .var("ENGINE_FUZZ_SEED")
        .unwrap_or_else(|| "1".into())
        .parse()
        .map_err(|_| io::Error::other("ENGINE_FUZZ_SEED must be an integer."))?;
    let build = if features.is_empty() {
        "default"
    } else {
        features
    };
    let revision = tools.output(root, "git", &["rev-parse", "--short", "HEAD"]);
    let parent = root.join(format!(
        "target/engine-exploration/fuzz/{target}/{build}/{seed}"
    ));
    let output = campaign_dir(&parent, &revision)?;
    println!(
        "Campaign evidence: {}",
        output.strip_prefix(root).unwrap_or(&output).display()
    );
    let (seeds, corpus_dir) = (
        root.join("fuzz/seeds").join(target),
        root.join("fuzz/corpus").join(target),
    );
    let mut tools = tools.clone();
    let feature_args: Vec<&str> = if features.is_empty() {
        vec![]
    } else {
        vec!["--features", features]
    };
    let mut stage = "seed-sync";
    let result = (|| -> Result<i32, (String, i32)> {
        sync_seeds(&seeds, &corpus_dir).map_err(|e| (e, 1))?;
        stage = "replay-settings";
        tools.vars.insert(
            "ENGINE_SEMANTIC_REPORT".into(),
            output.join("semantics").to_string_lossy().into_owned(),
        );
        let campaign = Campaign {
            target: target.into(),
            features: build.into(),
            seconds,
            timeout: deadline,
            max_len,
            seed,
        };
        begin(root, &output, &corpus_dir, &seeds, &campaign, &tools).map_err(|e| (e, 1))?;
        // Cargo-fuzz has no --locked build flag; reject lockfile drift before it builds.
        stage = "metadata";
        let metadata_log =
            fs::File::create(output.join("metadata.log")).map_err(|e| (e.to_string(), 1))?;
        let code = tools
            .command("cargo", root)
            .args([
                "metadata",
                "--manifest-path",
                "fuzz/Cargo.toml",
                "--locked",
                "--format-version",
                "1",
            ])
            .stdout(Stdio::null())
            .stderr(metadata_log)
            .status()
            .map_err(|e| (e.to_string(), 1))?
            .code()
            .unwrap_or(1);
        if code != 0 {
            return Err(("cargo metadata failed".into(), code));
        }
        // Prebuilt cargo-fuzz may be compiled for musl and inherit that as its
        // default. ASan needs the native Rust host target, which also owns the
        // calibration binary.
        let host = tools
            .output(root, "rustc", &["-vV"])
            .lines()
            .find_map(|l| l.strip_prefix("host: "))
            .unwrap_or("")
            .to_string();
        if host.is_empty() {
            return Err(("rustc did not report its host target.".into(), 1));
        }
        stage = "build";
        let mut command = tools.command("cargo", root);
        command
            .args(["fuzz", "build", "--fuzz-dir", "fuzz", "--target", &host])
            .args(&feature_args)
            .arg(target);
        let code = tee(command, &output.join("build.log")).map_err(|e| (e.to_string(), 1))?;
        if code != 0 {
            return Err(("cargo fuzz build failed".into(), code));
        }
        stage = "calibration";
        let mut calibration = tools.clone();
        calibration.vars.insert(
            "ENGINE_SEMANTIC_REPORT".into(),
            output
                .join("calibration-semantics")
                .to_string_lossy()
                .into_owned(),
        );
        let binary = root.join(format!("fuzz/target/{host}/release/{target}"));
        calibrate(&output, &seeds, &binary, deadline, &calibration).map_err(|e| (e, 1))?;
        stage = "exploration";
        let mut command = tools.command("cargo", root);
        command
            .args(["fuzz", "run", "--fuzz-dir", "fuzz", "--target", &host])
            .args(&feature_args)
            .args([target, "--"])
            .args([
                format!("-max_total_time={seconds}"),
                format!("-max_len={max_len}"),
                format!("-timeout={deadline}"),
                "-rss_limit_mb=4096".into(),
                format!("-seed={seed}"),
                "-print_final_stats=1".into(),
            ]);
        let fuzz_exit = tee(command, &output.join("fuzzer.log")).map_err(|e| (e.to_string(), 1))?;
        stage = "reporting";
        match finish(&output, &corpus_dir, fuzz_exit).map_err(|e| (e.to_string(), 1))? {
            Ok(()) => Ok(fuzz_exit),
            Err(reason) if fuzz_exit == 0 => Err((reason, 1)),
            Err(_) => Ok(fuzz_exit),
        }
    })();
    match result {
        Ok(code) => Ok(code),
        Err((problem, code)) => {
            eprintln!("{problem}");
            incomplete(&output, stage, code);
            Ok(code)
        }
    }
}

/// `report.json` for a property run: the toolchain, the cases and seed it
/// ran with, and the scenario observations it left in `output`.
fn properties_report(root: &Path, output: &Path, tools: &Tools) -> io::Result<()> {
    fs::create_dir_all(output)?;
    let (cases, counts) = semantics(output);
    let settings: Map<String, Value> = [
        "PROPTEST_CASES",
        "ENGINE_PROOF_CASES",
        "PROPTEST_RNG_SEED",
        "ENGINE_FEATURES",
    ]
    .iter()
    .map(|n| (n.to_string(), json!(tools.var(n))))
    .collect();
    write_json(
        &output.join("report.json"),
        &json!({
            "schema": 2, "revision": tools.output(root, "git", &["rev-parse", "HEAD"]),
            "rustc": tools.output(root, "rustc", &["-Vv"]), "cargo": tools.output(root, "cargo", &["-V"]),
            "nextest": tools.output(root, "cargo", &["nextest", "--version"]),
            "settings": settings, "semantic_cases": cases, "semantic_observations": counts,
        }),
    )
}

/// The build named on the command line: `default` or `zmq`.
fn build_of(args: &[&str], command: &str) -> io::Result<&'static str> {
    match args {
        [] | ["default"] | [""] => Ok("default"),
        ["zmq"] => Ok("zmq"),
        _ => Err(io::Error::other(format!(
            "usage: cargo xtask engine {command} [default|zmq]"
        ))),
    }
}

/// `cargo xtask engine properties`: the property tests of one build, then
/// their report, written whether or not they passed. Returns their exit code.
pub(crate) fn properties(root: &Path, args: &[&str], tools: &Tools) -> io::Result<i32> {
    let build = build_of(args, "properties")?;
    let output = root.join("target/engine-exploration/properties");
    fs::create_dir_all(&output)?;
    let mut tools = tools.clone();
    tools.vars.insert(
        "ENGINE_FEATURES".into(),
        if build == "zmq" {
            "zmq".into()
        } else {
            String::new()
        },
    );
    if tools.var("ENGINE_SEMANTIC_REPORT").is_none() {
        tools.vars.insert(
            "ENGINE_SEMANTIC_REPORT".into(),
            output.join("semantics").display().to_string(),
        );
    }
    let toolchain = [
        tools.output(root, "rustc", &["-Vv"]),
        tools.output(root, "cargo", &["-V"]),
        tools.output(root, "cargo", &["nextest", "--version"]),
        tools.output(root, "git", &["rev-parse", "HEAD"]),
    ];
    fs::write(output.join("toolchain.txt"), toolchain.join("\n") + "\n")?;
    let unset = || "unset".to_string();
    let settings = format!(
        "Cases per property: {}; proof cases: {}; RNG seed: {}; features: {build}",
        tools.var("PROPTEST_CASES").unwrap_or_else(unset),
        tools.var("ENGINE_PROOF_CASES").unwrap_or_else(unset),
        tools.var("PROPTEST_RNG_SEED").unwrap_or_else(unset)
    );
    println!("{settings}");
    if let Some(summary) = tools.var("GITHUB_STEP_SUMMARY") {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(summary)?;
        writeln!(file, "{settings}")?;
    }
    let mut command = tools.command("cargo", root);
    command.args([
        "nextest",
        "run",
        "-p",
        "engine",
        "-p",
        "key-custody",
        "--lib",
        "--locked",
        "--profile",
        "ci",
    ]);
    if build == "zmq" {
        command.args(["--features", "engine/zmq"]);
    }
    let code = command
        .args(["-E", "test(::properties::)"])
        .status()?
        .code()
        .unwrap_or(1);
    properties_report(root, &output, &tools)?;
    Ok(code)
}

/// `cargo xtask engine scale`: generated cases and every explicit large
/// fixture. One test at a time in separate processes, so the scale suite
/// doesn't measure its own competition. Returns the tests' exit code.
pub(crate) fn scale(root: &Path, args: &[&str], tools: &Tools) -> io::Result<i32> {
    let build = build_of(args, "scale")?;
    let mut tools = tools.clone();
    let cases = tools.var("PROPTEST_CASES").unwrap_or_else(|| "32".into());
    let seed = tools
        .var("PROPTEST_RNG_SEED")
        .unwrap_or_else(|| "24601".into());
    tools.vars.insert("PROPTEST_CASES".into(), cases.clone());
    tools.vars.insert("PROPTEST_RNG_SEED".into(), seed.clone());
    let output = root.join("target/engine-scale").join(build);
    fs::create_dir_all(&output)?;
    fs::write(
        output.join("replay.txt"),
        format!(
            "revision={}\nfeatures={build} cases={cases} seed={seed}\n{}\n",
            tools.output(root, "git", &["rev-parse", "HEAD"]),
            tools.output(root, "rustc", &["--version"])
        ),
    )?;
    let mut command = tools.command("cargo", root);
    command.args([
        "nextest",
        "run",
        "-p",
        "engine",
        "--lib",
        "--locked",
        "--profile",
        "ci",
        "--run-ignored",
        "all",
        "--test-threads",
        "1",
    ]);
    if build == "zmq" {
        command.args(["--features", "zmq"]);
    }
    command.args(["-E", "test(::scale::)"]);
    let code = tee(command, &output.join("tests.log"))?;
    let target = tools
        .var("CARGO_TARGET_DIR")
        .map_or_else(|| root.join("target"), |t| root.join(t));
    let junit = target.join("nextest/ci/junit.xml");
    if junit.is_file() {
        fs::copy(junit, output.join("junit.xml"))?;
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                env::temp_dir().join(format!("xtask-exploration-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_complete_libfuzzer_campaign() {
        let result = summarize("#8 INITED cov: 10 ft: 12 corp: 3/10b\n#42 NEW cov: 13 ft: 19\n#55 DONE cov: 13 ft: 20\nstat::number_of_executed_units: 55\nstat::average_exec_per_sec: 5\n");
        assert_eq!(result["coverage_growth"], 3);
        assert_eq!(result["executions_after_initialization"], 47);
        assert_eq!(result["feature_growth"], 8);
        assert_eq!(result["stats"]["number_of_executed_units"], 55);
    }

    #[test]
    fn no_coverage_claim_for_an_uninstrumented_seed_replay() {
        let result = summarize("Running seed once\nDone\n");
        assert!(result["coverage_growth"].is_null());
        assert!(result["final"].is_null());
    }

    #[test]
    fn a_failure_before_initialization_has_no_growth() {
        let result = summarize("#12 NEW cov: 9 ft: 10\nERROR: AddressSanitizer\n");
        assert!(result["coverage_growth"].is_null());
        assert_eq!(result["final"]["executions"], 12);
    }

    #[test]
    fn corpus_growth_counts_unique_contents() {
        let s = Scratch::new("corpus");
        fs::write(s.0.join("one"), b"same").unwrap();
        fs::write(s.0.join("two"), b"same").unwrap();
        fs::write(s.0.join("three"), b"new").unwrap();
        fs::create_dir(s.0.join("nested")).unwrap();
        let result = corpus(&s.0);
        assert_eq!(
            (result["files"].as_u64(), result["bytes"].as_u64()),
            (Some(3), Some(11))
        );
        assert_eq!(result["sha256"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn semantic_counts_aggregate_families_without_calibration() {
        let s = Scratch::new("semantics");
        fs::write(s.0.join("semantics.1.jsonl"), "{\"selected-command:Mine(2)\":1,\"applied-transition:Mine\":1,\"fixture:scanner-valid-synthetic\":1}\n").unwrap();
        fs::write(
            s.0.join("semantics.2.jsonl"),
            "{\"selected-command:Mine(8)\":2,\"skipped-command:Mine\":2}\n",
        )
        .unwrap();
        fs::write(
            s.0.join("calibration-semantics.3.jsonl"),
            "{\"command:Mine(0)\":999}\n",
        )
        .unwrap();
        let (cases, counts) = semantics(&s.0);
        assert_eq!(cases, 2);
        assert_eq!(counts["selected-command:Mine"], 3);
        assert_eq!(counts["applied-transition:Mine"], 1);
        assert_eq!(counts["skipped-command:Mine"], 2);
    }

    #[test]
    fn finish_rejects_missing_malformed_negative_zero_and_incomplete_evidence() {
        for (log, expected) in [
            ("Running seed once\nDone\n", "invalid-evidence"),
            (
                "#8 INITED cov: unknown ft: 12\n#55 DONE cov: 13 ft: 20\n",
                "invalid-evidence",
            ),
            (
                "#8 INITED cov: 10 ft: 12\n#7 DONE cov: 10 ft: 12\n",
                "invalid-evidence",
            ),
            (
                "#8 INITED cov: 10 ft: 12\n#8 DONE cov: 10 ft: 12\n",
                "insufficient-exploration",
            ),
            (
                "#8 INITED cov: 10 ft: 12\n#9 NEW cov: 11 ft: 13\n",
                "invalid-evidence",
            ),
            (
                "#8 INITED cov: 10 ft: 12\n#9 DONE cov: 11 ft: 13\n",
                "passed",
            ),
        ] {
            let s = Scratch::new(&format!("finish-{expected}-{}", log.len()));
            write_json(
                &s.0.join("replay.json"),
                &json!({"started": now(), "corpus": {"sha256": []}}),
            )
            .unwrap();
            fs::write(s.0.join("fuzzer.log"), log).unwrap();
            let verdict = finish(&s.0, &s.0.join("corpus"), 0).unwrap();
            assert_eq!(verdict.is_ok(), expected == "passed", "{log}");
            assert_eq!(
                read_json(&s.0.join("report.json")).unwrap()["status"],
                expected,
                "{log}"
            );
        }
    }

    #[test]
    fn empty_and_missing_calibration_seeds_fail_explicitly() {
        for exists in [false, true] {
            let s = Scratch::new(&format!("calibration-{exists}"));
            let seeds = s.0.join("seeds");
            if exists {
                fs::create_dir(&seeds).unwrap();
            }
            let report = s.0.join("report");
            fs::create_dir(&report).unwrap();
            assert!(calibrate(
                &report,
                &seeds,
                &s.0.join("unused-binary"),
                1,
                &Tools::default()
            )
            .is_err());
            let data = read_json(&report.join("calibration.json")).unwrap();
            assert_eq!(data["status"], "failed");
            assert_eq!(data["seeds"], json!([]));
        }
    }

    #[test]
    fn begin_refuses_previous_evidence_without_changing_it() {
        let s = Scratch::new("begin");
        fs::write(s.0.join("report.json"), "{\"status\":\"passed\"}").unwrap();
        let campaign = Campaign {
            target: "status".into(),
            features: "default".into(),
            seconds: 1,
            timeout: 1,
            max_len: 10,
            seed: 1,
        };
        assert!(begin(
            &s.0,
            &s.0,
            &s.0.join("corpus"),
            &s.0.join("seeds"),
            &campaign,
            &Tools::default()
        )
        .is_err());
        assert_eq!(
            fs::read_to_string(s.0.join("report.json")).unwrap(),
            "{\"status\":\"passed\"}"
        );
    }

    #[test]
    fn sync_seeds_keeps_discoveries_and_refreshes_changed_reviewed_contents() {
        let s = Scratch::new("sync");
        let (seeds, corpus_dir) = (s.0.join("seeds"), s.0.join("corpus"));
        fs::create_dir_all(&seeds).unwrap();
        fs::create_dir_all(&corpus_dir).unwrap();
        fs::write(seeds.join("named"), b"updated").unwrap();
        fs::write(corpus_dir.join("named"), b"cached old input").unwrap();
        fs::write(corpus_dir.join("discovery"), b"discovered").unwrap();
        sync_seeds(&seeds, &corpus_dir).unwrap();
        let reviewed = fs::read_dir(&corpus_dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("reviewed-")
            })
            .unwrap();
        assert_eq!(fs::read(&reviewed).unwrap(), b"updated");
        fs::write(&reviewed, b"changed by an earlier campaign").unwrap();
        sync_seeds(&seeds, &corpus_dir).unwrap();
        assert_eq!(fs::read(&reviewed).unwrap(), b"updated");
        assert_eq!(
            fs::read(corpus_dir.join("discovery")).unwrap(),
            b"discovered"
        );
    }

    /// A repository with reviewed seeds and fake git, rustc and cargo on the
    /// PATH, so a whole campaign runs in a second.
    #[cfg(unix)]
    mod runner {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        const CARGO: &str = r#"#!/bin/sh
failure="$ENGINE_TEST_FAILURE"
case "$1 $2" in
  "fuzz build"|"fuzz run")
    if [ -n "$ENGINE_TEST_MUSL_DEFAULT" ]; then
      found=""; previous=""
      for argument in "$@"; do
        if [ "$previous" = "--target" ]; then found="$argument"; fi
        previous="$argument"
      done
      if [ "$found" != "x86_64-unknown-linux-gnu" ]; then
        echo "sanitizer is incompatible with statically linked libc" >&2; exit 42
      fi
    fi;;
esac
if [ "$1" = metadata ]; then
  if [ "$failure" = metadata ]; then exit 17; fi; exit 0
fi
if [ "$1 $2" = "fuzz build" ]; then
  if [ "$failure" = build ]; then exit 19; fi
  mkdir -p fuzz/target/x86_64-unknown-linux-gnu/release
  printf '#!/bin/sh\nif [ "$ENGINE_TEST_FAILURE" = calibration ]; then exit 23; fi\nexit 0\n' > fuzz/target/x86_64-unknown-linux-gnu/release/status
  chmod +x fuzz/target/x86_64-unknown-linux-gnu/release/status
elif [ "$1 $2" = "fuzz run" ]; then
  echo '#8 INITED cov: 10 ft: 12'
  echo '#55 DONE cov: 13 ft: 20'
  if [ "$failure" = exploration ]; then exit 41; fi
else
  echo 'test tool version'
fi
"#;

        struct Repo {
            scratch: Scratch,
            tools: Tools,
        }

        impl Repo {
            fn new(name: &str) -> Self {
                let scratch = Scratch::new(name);
                let root = &scratch.0;
                fs::write(root.join("Cargo.lock"), "root lock").unwrap();
                fs::create_dir_all(root.join("fuzz/seeds/status")).unwrap();
                fs::write(root.join("fuzz/seeds/status/boundary"), b"reviewed").unwrap();
                fs::write(root.join("fuzz/Cargo.lock"), "fuzz lock").unwrap();
                let bin = root.join("bin");
                fs::create_dir(&bin).unwrap();
                for (name, body) in [
                    ("git", "#!/bin/sh\ncase \"$1\" in rev-parse) echo abcdef0123456789;; status) ;; esac\n"),
                    ("rustc", "#!/bin/sh\necho \"rustc test compiler\"\necho \"host: x86_64-unknown-linux-gnu\"\n"),
                    ("cargo", CARGO),
                ] {
                    fs::write(bin.join(name), body).unwrap();
                    fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o755)).unwrap();
                }
                let mut tools = Tools::default();
                let path = format!("{}:{}", bin.display(), env::var("PATH").unwrap_or_default());
                tools.vars.extend([
                    ("PATH".into(), path),
                    ("RUSTUP_TOOLCHAIN".into(), "fixture".into()),
                    ("ENGINE_FUZZ_SEED".into(), "1".into()),
                ]);
                Repo { scratch, tools }
            }
            fn campaign(&self, failure: Option<&str>) -> i32 {
                let mut tools = self.tools.clone();
                if let Some(failure) = failure {
                    tools
                        .vars
                        .insert("ENGINE_TEST_FAILURE".into(), failure.into());
                }
                fuzz(&self.scratch.0, &["status", "1"], &tools).unwrap()
            }
            fn reports(&self) -> Vec<PathBuf> {
                let dir = self
                    .scratch
                    .0
                    .join("target/engine-exploration/fuzz/status/default/1");
                let mut found: Vec<PathBuf> = fs::read_dir(dir)
                    .unwrap()
                    .flatten()
                    .map(|e| e.path().join("report.json"))
                    .filter(|p| p.is_file())
                    .collect();
                found.sort();
                found
            }
        }

        #[test]
        fn same_seed_runs_have_independent_evidence_even_when_the_second_build_fails() {
            let repo = Repo::new("independent");
            assert_eq!(repo.campaign(None), 0);
            let original = repo.reports()[0].clone();
            let content = fs::read(&original).unwrap();
            assert_eq!(repo.campaign(Some("build")), 19);
            assert_eq!(repo.reports().len(), 2);
            assert_eq!(fs::read(&original).unwrap(), content);
            let other = repo.reports().into_iter().find(|p| *p != original).unwrap();
            let other = read_json(&other).unwrap();
            assert_eq!(
                (other["stage"].as_str(), other["status"].as_str()),
                (Some("build"), Some("failed"))
            );
        }

        #[test]
        fn a_prebuilt_musl_fuzzer_uses_the_rustc_host_for_build_and_run() {
            let mut repo = Repo::new("musl");
            repo.tools
                .vars
                .insert("ENGINE_TEST_MUSL_DEFAULT".into(), "1".into());
            assert_eq!(repo.campaign(None), 0);
            let data = read_json(&repo.reports()[0]).unwrap();
            assert_eq!(data["status"], "passed");
            assert_eq!(data["exploration"]["executions_after_initialization"], 47);
        }

        #[test]
        fn metadata_calibration_and_exploration_failures_are_terminal() {
            let repo = Repo::new("terminal");
            for (stage, code) in [("metadata", 17), ("calibration", 1), ("exploration", 41)] {
                assert_eq!(repo.campaign(Some(stage)), code, "{stage}");
            }
            let data: Vec<Value> = repo
                .reports()
                .iter()
                .map(|p| read_json(p).unwrap())
                .collect();
            assert_eq!(data.len(), 3);
            assert!(data.iter().all(|d| d["status"] == "failed"));
            assert!(data.iter().any(|d| d["stage"] == "calibration"));
            let exploration = data.iter().find(|d| d["exit_code"] == 41).unwrap();
            assert_eq!(
                exploration["exploration"]["executions_after_initialization"],
                47
            );
            assert!(exploration.get("stage").is_none());
        }
    }
}
