//! Engine verification runs: `cargo xtask engine fuzz` runs one
//! coverage-guided fuzz campaign and records its replay settings and measured
//! exploration; `engine properties` and `engine scale` run the property and
//! scale tests, the property run reporting the scenarios it reached. None of
//! them infers line coverage: they record what the fuzzer and the tests
//! report (`docs/ENGINE_VERIFICATION.md`).

use crate::support::{at, read_json, wait_with_deadline, write_json, Exit, OnTimeout};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fmt, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::LazyLock,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

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

/// An engine build the explorations run against: the default features, or
/// with the ZMQ node feed. Each run records which, and the reports are
/// filed by it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Build {
    Default,
    Zmq,
}

impl Build {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Build::Default => "default",
            Build::Zmq => "zmq",
        }
    }

    /// The feature list the build is made with, as `ENGINE_FEATURES` and
    /// cargo's `--features` take it.
    pub(crate) fn features(self) -> &'static str {
        match self {
            Build::Default => "",
            Build::Zmq => "zmq",
        }
    }

    /// `default`, `zmq`, or nothing for the default build.
    pub(crate) fn parse(name: &str) -> Option<Self> {
        match name {
            "" | "default" => Some(Build::Default),
            "zmq" => Some(Build::Zmq),
            _ => None,
        }
    }
}

impl fmt::Display for Build {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A campaign's verdict in report.json; `running` until it has one.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Status {
    Running,
    Passed,
    Failed,
    InvalidEvidence,
    InsufficientExploration,
}

impl Status {
    fn is_terminal(self) -> bool {
        self != Status::Running
    }
}

/// The steps of a campaign, in order; a report of a campaign that stopped
/// early names the one it stopped at.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Stage {
    SeedSync,
    ReplaySettings,
    Metadata,
    Build,
    Calibration,
    Exploration,
    Reporting,
}

/// A target's budget, per-input deadline and input size limit: IO
/// histories need a larger deadline and sustained budget than policies.
#[derive(Clone, Copy)]
struct Limits {
    seconds: u64,
    timeout: u64,
    max_len: u64,
}

fn limits(target: &str) -> Option<Limits> {
    let (seconds, timeout, max_len) = match target {
        "history" => (900, 60, 4096),
        "portfolio" => (900, 120, 260),
        "notifications" | "queue" => (600, 60, 4096),
        "scheduler" | "resources" | "inputs" | "mempool" | "status" => (300, 10, 4096),
        _ => return None,
    };
    Some(Limits {
        seconds,
        timeout,
        max_len,
    })
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// The reviewed seeds of a target, which must exist.
fn seed_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let missing = |what: &str| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("Reviewed seed directory is {what}: {}", dir.display()),
        )
    };
    if !dir.is_dir() {
        return Err(missing("missing"));
    }
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| at(dir, e))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(missing("empty"));
    }
    Ok(files)
}

/// Copies the reviewed seeds into the corpus under their content's hash: a
/// reviewed input is identified by content, not its possibly cached name, and
/// the copy is refreshed even if an earlier campaign changed it.
fn sync_seeds(seeds: &Path, corpus: &Path) -> io::Result<()> {
    let files = seed_files(seeds)?;
    fs::create_dir_all(corpus).map_err(|e| at(corpus, e))?;
    for seed in files {
        let bytes = fs::read(&seed).map_err(|e| at(&seed, e))?;
        let copy = corpus.join(format!("reviewed-{}", sha256(&bytes)));
        fs::write(&copy, bytes).map_err(|e| at(&copy, e))?;
    }
    Ok(())
}

/// The corpus's files, bytes and distinct contents.
#[derive(Serialize, Deserialize, Default)]
struct Corpus {
    files: usize,
    bytes: u64,
    sha256: BTreeSet<String>,
}

fn corpus(dir: &Path) -> Corpus {
    let files: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    let mut summary = Corpus {
        files: files.len(),
        ..Corpus::default()
    };
    for file in &files {
        let content = fs::read(file).unwrap_or_default();
        summary.bytes += content.len() as u64;
        summary.sha256.insert(sha256(&content));
    }
    summary
}

static SAMPLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"#(\d+)\s+(INITED|NEW|REDUCE|DONE).*?cov:\s*(\d+)\s+ft:\s*(\d+)").unwrap()
});
static STAT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"stat::([a-z_]+):\s*(\d+)").unwrap());

/// One of libFuzzer's progress lines.
#[derive(Clone, Serialize)]
struct Sample {
    executions: u64,
    phase: String,
    coverage: u64,
    features: u64,
}

/// What libFuzzer's log says it explored: the first and last coverage
/// samples, its final stats, and the growth between them.
#[derive(Serialize)]
struct Exploration {
    initial: Option<Sample>,
    #[serde(rename = "final")]
    last: Option<Sample>,
    stats: BTreeMap<String, u64>,
    executions_after_initialization: Option<i64>,
    coverage_growth: Option<i64>,
    feature_growth: Option<i64>,
}

fn summarize(log: &str) -> Exploration {
    let number = |text: &str| text.parse::<u64>().unwrap_or(0);
    let samples: Vec<Sample> = log
        .lines()
        .filter_map(|line| SAMPLE.captures(line))
        .map(|c| Sample {
            executions: number(&c[1]),
            phase: c[2].to_string(),
            coverage: number(&c[3]),
            features: number(&c[4]),
        })
        .collect();
    let initial = samples.iter().find(|s| s.phase == "INITED").cloned();
    let last = samples.last().cloned();
    let stats = STAT
        .captures_iter(log)
        .map(|c| (c[1].to_string(), number(&c[2])))
        .collect();
    let delta = |field: fn(&Sample) -> u64| match (&initial, &last) {
        (Some(i), Some(f)) => Some(
            i64::try_from(field(f)).unwrap_or(i64::MAX)
                - i64::try_from(field(i)).unwrap_or(i64::MAX),
        ),
        _ => None,
    };
    Exploration {
        executions_after_initialization: delta(|s| s.executions),
        coverage_growth: delta(|s| s.coverage),
        feature_growth: delta(|s| s.features),
        initial,
        last,
        stats,
    }
}

/// Whether a finished campaign's evidence counts, and why not.
struct Verdict {
    status: Status,
    reason: Option<&'static str>,
}

fn evidence_status(exploration: &Exploration, exit: Exit) -> Verdict {
    let verdict = |status, reason| Verdict {
        status,
        reason: Some(reason),
    };
    if !exit.succeeded() {
        return verdict(
            Status::Failed,
            "Fuzzer exited unsuccessfully; inspect the raw log.",
        );
    }
    let delta = exploration.executions_after_initialization;
    let done = exploration.last.as_ref().is_some_and(|s| s.phase == "DONE");
    if exploration.initial.is_none() || !done || delta.is_none_or(|d| d < 0) {
        return verdict(
            Status::InvalidEvidence,
            "Missing, malformed or inconsistent completed exploration evidence.",
        );
    }
    if delta == Some(0) {
        return verdict(
            Status::InsufficientExploration,
            "No inputs explored after initialization; increase the budget.",
        );
    }
    Verdict {
        status: Status::Passed,
        reason: None,
    }
}

/// The scenario observations the engine wrote while it ran (one JSON line
/// per case), added up; command names lose their arguments. A line that
/// isn't a JSON object of counts is an error: the evidence is only as good
/// as its weakest file.
fn semantics(dir: &Path) -> io::Result<(u64, BTreeMap<String, u64>)> {
    let mut counts = BTreeMap::new();
    let mut cases = 0;
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok((0, counts));
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(name.starts_with("semantics.") && name.ends_with(".jsonl")) {
            continue;
        }
        let path = entry.path();
        let text = fs::read_to_string(&path).map_err(|e| at(&path, e))?;
        for (n, line) in text.lines().enumerate() {
            let map: Map<String, Value> = serde_json::from_str(line).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}:{}: {e}", path.display(), n + 1),
                )
            })?;
            cases += 1;
            for (mut key, count) in map {
                if key.starts_with("command:") || key.starts_with("selected-command:") {
                    key = key.split(['(', '{', ' ']).next().unwrap_or("").to_string();
                }
                *counts.entry(key).or_insert(0) += count.as_u64().unwrap_or(0);
            }
        }
    }
    Ok((cases, counts))
}

/// Programs and their environment, so tests can stand fakes in for git,
/// rustc and cargo.
#[derive(Clone, Default)]
pub(crate) struct Tools {
    vars: BTreeMap<String, String>,
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

    /// What a program prints, trimmed; a program that can't run or fails is
    /// an error, since what it would have said goes in the evidence.
    fn output(&self, root: &Path, program: &str, args: &[&str]) -> io::Result<String> {
        let output = self
            .command(program, root)
            .args(args)
            .output()
            .map_err(|e| io::Error::new(e.kind(), format!("{program}: {e}")))?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "{program} {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

/// The target rustc builds for, from `rustc -vV`.
fn host_target(rustc: &str) -> io::Result<String> {
    rustc
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(str::to_string)
        .ok_or_else(|| io::Error::other("rustc did not report its host target."))
}

/// Runs a program to a log file and our own output at once (`2>&1 | tee`),
/// returning its exit code.
fn tee(mut command: Command, log: &Path) -> io::Result<Exit> {
    let (mut reader, writer) = io::pipe()?;
    let mut child = command.stdout(writer.try_clone()?).stderr(writer).spawn()?;
    // The command holds the pipe's write ends; dropping it leaves the child
    // as their only owner, so the read below ends when the child does.
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
    Ok(Exit::of(child.wait()?))
}

/// One seed's replay against the built fuzzer.
#[derive(Serialize)]
struct SeedRun {
    seed: String,
    seconds: f64,
    exit_code: Exit,
}

/// Runs every reviewed seed once against the built fuzzer, each under a
/// watchdog, so a slow or crashing seed stops the campaign before it starts.
/// calibration.json records the outcome either way.
fn calibrate(
    output: &Path,
    seeds: &Path,
    binary: &Path,
    timeout: u64,
    tools: &Tools,
) -> io::Result<()> {
    let calibration = output.join("calibration.json");
    let files = match seed_files(seeds) {
        Ok(files) => files,
        Err(problem) => {
            write_json(
                &calibration,
                &json!({"status": Status::Failed, "reason": problem.to_string(), "seeds": []}),
            )?;
            return Err(problem);
        }
    };
    let mut results = Vec::new();
    for seed in &files {
        let name = seed
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let log = output.join(format!("calibration-{name}.log"));
        let started = Instant::now();
        let file = fs::File::create(&log).map_err(|e| at(&log, e))?;
        let exit_code = match Command::new(binary.canonicalize().unwrap_or(binary.to_path_buf()))
            .args(["-runs=1", "-seed=1", &format!("-timeout={timeout}")])
            .arg(seed.canonicalize().unwrap_or(seed.clone()))
            .envs(&tools.vars)
            .stdout(file.try_clone()?)
            .stderr(file)
            .spawn()
        {
            Err(e) => {
                fs::write(&log, format!("{e}\n")).ok();
                Exit::NOT_FOUND
            }
            Ok(mut child) => {
                let status = wait_with_deadline(
                    &mut child,
                    Duration::from_secs(timeout + 10),
                    OnTimeout::KillChild,
                )?;
                status.map_or_else(
                    || {
                        fs::write(&log, "Seed exceeded calibration watchdog\n").ok();
                        Exit::TIMED_OUT
                    },
                    Exit::of,
                )
            }
        };
        results.push(SeedRun {
            seed: name,
            seconds: started.elapsed().as_secs_f64(),
            exit_code,
        });
    }
    let failed = results.iter().any(|r| !r.exit_code.succeeded());
    let mut durations: Vec<f64> = results.iter().map(|r| r.seconds).collect();
    durations.sort_by(f64::total_cmp);
    let n = durations.len();
    let median = if n % 2 == 1 {
        durations[n / 2]
    } else {
        f64::midpoint(durations[n / 2 - 1], durations[n / 2])
    };
    write_json(
        &calibration,
        &json!({
            "status": if failed { Status::Failed } else { Status::Passed },
            "binary_sha256": sha256(&fs::read(binary).unwrap_or_default()),
            "seeds": results, "median_seconds": median, "max_seconds": durations[n - 1], "timeout_seconds": timeout,
        }),
    )?;
    if failed {
        return Err(io::Error::other("Seed calibration failed; inspect logs."));
    }
    Ok(())
}

/// A campaign's settings: what replaying it needs.
#[derive(Serialize)]
struct Campaign<'a> {
    target: &'a str,
    features: Build,
    seconds: u64,
    timeout: u64,
    max_len: u64,
    seed: u64,
}

/// Records the replay settings in a new, empty campaign folder; returns the
/// host target the fuzzer is built for.
fn begin(
    root: &Path,
    output: &Path,
    corpus_dir: &Path,
    seeds: &Path,
    c: &Campaign,
    tools: &Tools,
) -> io::Result<String> {
    if fs::read_dir(output)
        .map_err(|e| at(output, e))?
        .next()
        .is_some()
    {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Refusing to reuse a nonempty campaign directory.",
        ));
    }
    write_json(
        &output.join("report.json"),
        &json!({"schema": 2, "status": Status::Running}),
    )?;
    let rustc = tools.output(root, "rustc", &["-Vv"])?;
    let host = host_target(&rustc)?;
    let toolchain = match tools.var("RUSTUP_TOOLCHAIN") {
        Some(named) => named,
        None => tools.output(root, "rustup", &["show", "active-toolchain"])?,
    };
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
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
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
            "settings": c,
            "revision": tools.output(root, "git", &["rev-parse", "HEAD"])?,
            "dirty": !tools.output(root, "git", &["status", "--porcelain"])?.is_empty(),
            "platform": format!("{}-{}", env::consts::OS, env::consts::ARCH),
            "rustc": rustc, "fuzz_target_triple": host,
            "cargo": tools.output(root, "cargo", &["-V"])?, "cargo_fuzz": tools.output(root, "cargo", &["fuzz", "--version"])?,
            "toolchain": toolchain, "sanitizer_environment": sanitizers, "lock_sha256": locks,
            "reviewed_seeds": reviewed, "started": now(), "corpus": corpus(corpus_dir),
        }),
    )?;
    Ok(host)
}

/// Marks a campaign that stopped before reporting as failed at `stage`,
/// unless it already reached a verdict.
fn incomplete(output: &Path, stage: Stage, exit: Exit) {
    let path = output.join("report.json");
    let current = read_json::<Value>(&path)
        .ok()
        .and_then(|report| serde_json::from_value::<Status>(report["status"].clone()).ok());
    if !current.is_some_and(Status::is_terminal) {
        let _ = write_json(
            &path,
            &json!({"schema": 2, "status": Status::Failed, "stage": stage, "exit_code": exit,
                "reason": "Campaign stopped before completed exploration reporting."}),
        );
    }
}

/// The finished campaign's report: its verdict, corpus growth, measured
/// exploration and scenario observations.
fn finish(output: &Path, corpus_dir: &Path, exit: Exit) -> io::Result<Verdict> {
    let before: Value = read_json(&output.join("replay.json"))?;
    let after = corpus(corpus_dir);
    let (cases, counts) = semantics(output)?;
    let log = output.join("fuzzer.log");
    let exploration = summarize(&fs::read_to_string(&log).map_err(|e| at(&log, e))?);
    let verdict = evidence_status(&exploration, exit);
    let known: BTreeSet<&str> = before["corpus"]["sha256"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let new = after
        .sha256
        .iter()
        .filter(|h| !known.contains(h.as_str()))
        .count();
    let data = json!({
        "schema": 2, "status": verdict.status, "reason": verdict.reason, "exit_code": exit,
        "wall_seconds": now() - before["started"].as_f64().unwrap_or(0.0), "corpus": after, "new_unique_inputs": new,
        "exploration": exploration, "semantic_cases": cases, "semantic_observations": counts,
    });
    write_json(&output.join("report.json"), &data)?;
    println!("{}", serde_json::to_string_pretty(&data)?);
    Ok(verdict)
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
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
}

/// Why a campaign stopped before it could report: the stage, the problem,
/// and the code the campaign ends with (the failed step's own, where it has one).
struct Stopped {
    stage: Stage,
    problem: String,
    exit: Exit,
}

impl Stopped {
    fn at(stage: Stage, exit: Exit) -> impl FnOnce(io::Error) -> Stopped {
        move |e| Stopped {
            stage,
            problem: e.to_string(),
            exit,
        }
    }
}

/// Everything a campaign needs to run, once its folder exists.
struct Plan<'a> {
    root: &'a Path,
    output: &'a Path,
    seeds: PathBuf,
    corpus_dir: PathBuf,
    campaign: Campaign<'a>,
    tools: Tools,
}

/// Cargo-fuzz has no --locked build flag; rejects lockfile drift before it builds.
fn check_lockfile(plan: &Plan) -> Result<(), Stopped> {
    let fail = |e: io::Error| Stopped::at(Stage::Metadata, Exit::FAILURE)(e);
    let log = plan.output.join("metadata.log");
    let metadata_log = fs::File::create(&log).map_err(|e| fail(at(&log, e)))?;
    let status = plan
        .tools
        .command("cargo", plan.root)
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
        .map_err(fail)?;
    let exit = Exit::of(status);
    if exit.succeeded() {
        Ok(())
    } else {
        Err(Stopped {
            stage: Stage::Metadata,
            problem: "cargo metadata failed".into(),
            exit,
        })
    }
}

/// `cargo fuzz build` or `cargo fuzz run` for the campaign's target.
fn cargo_fuzz(plan: &Plan, verb: &str, host: &str) -> Command {
    let mut command = plan.tools.command("cargo", plan.root);
    command.args(["fuzz", verb, "--fuzz-dir", "fuzz", "--target", host]);
    if plan.campaign.features != Build::Default {
        command.args(["--features", plan.campaign.features.features()]);
    }
    command.arg(plan.campaign.target);
    command
}

/// The campaign's stages in order; `Err` says where it stopped.
fn run_campaign(plan: &Plan) -> Result<Exit, Stopped> {
    let c = &plan.campaign;
    sync_seeds(&plan.seeds, &plan.corpus_dir)
        .map_err(Stopped::at(Stage::SeedSync, Exit::FAILURE))?;
    let host = begin(
        plan.root,
        plan.output,
        &plan.corpus_dir,
        &plan.seeds,
        c,
        &plan.tools,
    )
    .map_err(Stopped::at(Stage::ReplaySettings, Exit::FAILURE))?;
    check_lockfile(plan)?;
    // Prebuilt cargo-fuzz may be compiled for musl and inherit that as its
    // default. ASan needs the native Rust host target, which also owns the
    // calibration binary.
    let built = tee(
        cargo_fuzz(plan, "build", &host),
        &plan.output.join("build.log"),
    )
    .map_err(Stopped::at(Stage::Build, Exit::FAILURE))?;
    if !built.succeeded() {
        return Err(Stopped {
            stage: Stage::Build,
            problem: "cargo fuzz build failed".into(),
            exit: built,
        });
    }
    let mut calibration = plan.tools.clone();
    calibration.vars.insert(
        "ENGINE_SEMANTIC_REPORT".into(),
        plan.output
            .join("calibration-semantics")
            .to_string_lossy()
            .into_owned(),
    );
    let binary = plan
        .root
        .join(format!("fuzz/target/{host}/release/{}", c.target));
    calibrate(plan.output, &plan.seeds, &binary, c.timeout, &calibration)
        .map_err(Stopped::at(Stage::Calibration, Exit::FAILURE))?;
    let mut explore = cargo_fuzz(plan, "run", &host);
    explore.arg("--").args([
        format!("-max_total_time={}", c.seconds),
        format!("-max_len={}", c.max_len),
        format!("-timeout={}", c.timeout),
        "-rss_limit_mb=4096".into(),
        format!("-seed={}", c.seed),
        "-print_final_stats=1".into(),
    ]);
    let fuzzer = tee(explore, &plan.output.join("fuzzer.log"))
        .map_err(Stopped::at(Stage::Exploration, Exit::FAILURE))?;
    let verdict = finish(plan.output, &plan.corpus_dir, fuzzer)
        .map_err(Stopped::at(Stage::Reporting, Exit::FAILURE))?;
    // The fuzzer's own failure is the code to pass on; evidence that fell
    // short of a pass after a clean run is this command's failure.
    if verdict.status == Status::Passed || !fuzzer.succeeded() {
        Ok(fuzzer)
    } else {
        Err(Stopped {
            stage: Stage::Reporting,
            problem: verdict.reason.unwrap_or_default().to_string(),
            exit: Exit::FAILURE,
        })
    }
}

/// One fuzz campaign from the repository at `root`; returns the exit code
/// the campaign ends with (the fuzzer's, or the failed step's).
pub(crate) fn fuzz(root: &Path, args: &[&str], tools: &Tools) -> io::Result<Exit> {
    let usage = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Target must be scheduler, resources, inputs, queue, mempool, status, history, notifications, or portfolio.",
        )
    };
    let (target, seconds, features) = match args {
        [t] => (*t, None, ""),
        [t, s] => (*t, Some(*s), ""),
        [t, s, f] => (*t, Some(*s), *f),
        _ => return Err(usage()),
    };
    let limits = limits(target).ok_or_else(usage)?;
    let seconds = match seconds {
        None => limits.seconds,
        Some(s) => s.parse().ok().filter(|s| *s > 0).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Seconds must be a positive integer.",
            )
        })?,
    };
    let build = Build::parse(features).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Features must be empty or zmq.",
        )
    })?;
    // cargo-fuzz passes -seed through to libFuzzer. Every run prints it for replay.
    let seed: u64 = tools
        .var("ENGINE_FUZZ_SEED")
        .unwrap_or_else(|| "1".into())
        .parse()
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "ENGINE_FUZZ_SEED must be an integer.",
            )
        })?;
    let revision = tools.output(root, "git", &["rev-parse", "--short", "HEAD"])?;
    let parent = root.join(format!(
        "target/engine-exploration/fuzz/{target}/{build}/{seed}"
    ));
    let output = campaign_dir(&parent, &revision)?;
    println!(
        "Campaign evidence: {}",
        output.strip_prefix(root).unwrap_or(&output).display()
    );
    let mut tools = tools.clone();
    tools.vars.insert(
        "ENGINE_SEMANTIC_REPORT".into(),
        output.join("semantics").to_string_lossy().into_owned(),
    );
    let plan = Plan {
        root,
        output: &output,
        seeds: root.join("fuzz/seeds").join(target),
        corpus_dir: root.join("fuzz/corpus").join(target),
        campaign: Campaign {
            target,
            features: build,
            seconds,
            timeout: limits.timeout,
            max_len: limits.max_len,
            seed,
        },
        tools,
    };
    match run_campaign(&plan) {
        Ok(exit) => Ok(exit),
        Err(stopped) => {
            eprintln!("{}", stopped.problem);
            incomplete(&output, stopped.stage, stopped.exit);
            Ok(stopped.exit)
        }
    }
}

/// `report.json` for a property run: the toolchain, the cases and seed it
/// ran with, and the scenario observations it left in `output`.
fn properties_report(root: &Path, output: &Path, tools: &Tools) -> io::Result<()> {
    fs::create_dir_all(output)?;
    let (cases, counts) = semantics(output)?;
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
            "schema": 2, "revision": tools.output(root, "git", &["rev-parse", "HEAD"])?,
            "rustc": tools.output(root, "rustc", &["-Vv"])?, "cargo": tools.output(root, "cargo", &["-V"])?,
            "nextest": tools.output(root, "cargo", &["nextest", "--version"])?,
            "settings": settings, "semantic_cases": cases, "semantic_observations": counts,
        }),
    )
}

/// The build named on the command line: `default` or `zmq`.
fn build_of(args: &[&str], command: &str) -> io::Result<Build> {
    match args {
        [] => Some(Build::Default),
        [name] => Build::parse(name),
        _ => None,
    }
    .ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("usage: cargo xtask engine {command} [default|zmq]"),
        )
    })
}

/// `cargo xtask engine properties`: the property tests of one build, then
/// their report, written whether or not they passed. Returns their exit code.
pub(crate) fn properties(root: &Path, args: &[&str], tools: &Tools) -> io::Result<Exit> {
    let build = build_of(args, "properties")?;
    let output = root.join("target/engine-exploration/properties");
    fs::create_dir_all(&output)?;
    let mut tools = tools.clone();
    tools
        .vars
        .insert("ENGINE_FEATURES".into(), build.features().into());
    if tools.var("ENGINE_SEMANTIC_REPORT").is_none() {
        tools.vars.insert(
            "ENGINE_SEMANTIC_REPORT".into(),
            output.join("semantics").display().to_string(),
        );
    }
    let toolchain = [
        tools.output(root, "rustc", &["-Vv"])?,
        tools.output(root, "cargo", &["-V"])?,
        tools.output(root, "cargo", &["nextest", "--version"])?,
        tools.output(root, "git", &["rev-parse", "HEAD"])?,
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
    if build == Build::Zmq {
        command.args(["--features", "engine/zmq"]);
    }
    let exit = Exit::of(command.args(["-E", "test(::properties::)"]).status()?);
    properties_report(root, &output, &tools)?;
    Ok(exit)
}

/// `cargo xtask engine scale`: generated cases and every explicit large
/// fixture. One test at a time in separate processes, so the scale suite
/// doesn't measure its own competition. Returns the tests' exit code; the
/// `JUnit` report nextest leaves is part of the evidence, so it must exist.
pub(crate) fn scale(root: &Path, args: &[&str], tools: &Tools) -> io::Result<Exit> {
    let build = build_of(args, "scale")?;
    let mut tools = tools.clone();
    let cases = tools.var("PROPTEST_CASES").unwrap_or_else(|| "32".into());
    let seed = tools
        .var("PROPTEST_RNG_SEED")
        .unwrap_or_else(|| "24601".into());
    tools.vars.insert("PROPTEST_CASES".into(), cases.clone());
    tools.vars.insert("PROPTEST_RNG_SEED".into(), seed.clone());
    let output = root.join("target/engine-scale").join(build.name());
    fs::create_dir_all(&output)?;
    fs::write(
        output.join("replay.txt"),
        format!(
            "revision={}\nfeatures={build} cases={cases} seed={seed}\n{}\n",
            tools.output(root, "git", &["rev-parse", "HEAD"])?,
            tools.output(root, "rustc", &["--version"])?
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
    if build == Build::Zmq {
        command.args(["--features", "zmq"]);
    }
    command.args(["-E", "test(::scale::)"]);
    let exit = tee(command, &output.join("tests.log"))?;
    let target = tools
        .var("CARGO_TARGET_DIR")
        .map_or_else(|| root.join("target"), |t| root.join(t));
    let junit = target.join("nextest/ci/junit.xml");
    fs::copy(&junit, output.join("junit.xml")).map_err(|e| at(&junit, e))?;
    Ok(exit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::Scratch;

    #[test]
    fn a_complete_libfuzzer_campaign() {
        let result = summarize("#8 INITED cov: 10 ft: 12 corp: 3/10b\n#42 NEW cov: 13 ft: 19\n#55 DONE cov: 13 ft: 20\nstat::number_of_executed_units: 55\nstat::average_exec_per_sec: 5\n");
        assert_eq!(result.coverage_growth, Some(3));
        assert_eq!(result.executions_after_initialization, Some(47));
        assert_eq!(result.feature_growth, Some(8));
        assert_eq!(result.stats["number_of_executed_units"], 55);
    }

    #[test]
    fn no_coverage_claim_for_an_uninstrumented_seed_replay() {
        let result = summarize("Running seed once\nDone\n");
        assert!(result.coverage_growth.is_none());
        assert!(result.last.is_none());
    }

    #[test]
    fn a_failure_before_initialization_has_no_growth() {
        let result = summarize("#12 NEW cov: 9 ft: 10\nERROR: AddressSanitizer\n");
        assert!(result.coverage_growth.is_none());
        assert_eq!(result.last.map(|s| s.executions), Some(12));
    }

    #[test]
    fn corpus_growth_counts_unique_contents() {
        let s = Scratch::new("exploration-corpus");
        fs::write(s.join("one"), b"same").unwrap();
        fs::write(s.join("two"), b"same").unwrap();
        fs::write(s.join("three"), b"new").unwrap();
        fs::create_dir(s.join("nested")).unwrap();
        let result = corpus(s.path());
        assert_eq!((result.files, result.bytes), (3, 11));
        assert_eq!(result.sha256.len(), 2);
    }

    #[test]
    fn semantic_counts_aggregate_families_without_calibration() {
        let s = Scratch::new("exploration-semantics");
        fs::write(s.join("semantics.1.jsonl"), "{\"selected-command:Mine(2)\":1,\"applied-transition:Mine\":1,\"fixture:scanner-valid-synthetic\":1}\n").unwrap();
        fs::write(
            s.join("semantics.2.jsonl"),
            "{\"selected-command:Mine(8)\":2,\"skipped-command:Mine\":2}\n",
        )
        .unwrap();
        fs::write(
            s.join("calibration-semantics.3.jsonl"),
            "{\"command:Mine(0)\":999}\n",
        )
        .unwrap();
        let (cases, counts) = semantics(s.path()).unwrap();
        assert_eq!(cases, 2);
        assert_eq!(counts["selected-command:Mine"], 3);
        assert_eq!(counts["applied-transition:Mine"], 1);
        assert_eq!(counts["skipped-command:Mine"], 2);
    }

    #[test]
    fn a_malformed_observation_line_fails_the_evidence() {
        let s = Scratch::new("exploration-malformed");
        fs::write(s.join("semantics.1.jsonl"), "{\"a\":1}\n[1,2]\n").unwrap();
        let error = semantics(s.path()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("semantics.1.jsonl:2"), "{error}");
    }

    #[test]
    fn finish_rejects_missing_malformed_negative_zero_and_incomplete_evidence() {
        for (log, expected) in [
            ("Running seed once\nDone\n", Status::InvalidEvidence),
            (
                "#8 INITED cov: unknown ft: 12\n#55 DONE cov: 13 ft: 20\n",
                Status::InvalidEvidence,
            ),
            (
                "#8 INITED cov: 10 ft: 12\n#7 DONE cov: 10 ft: 12\n",
                Status::InvalidEvidence,
            ),
            (
                "#8 INITED cov: 10 ft: 12\n#8 DONE cov: 10 ft: 12\n",
                Status::InsufficientExploration,
            ),
            (
                "#8 INITED cov: 10 ft: 12\n#9 NEW cov: 11 ft: 13\n",
                Status::InvalidEvidence,
            ),
            (
                "#8 INITED cov: 10 ft: 12\n#9 DONE cov: 11 ft: 13\n",
                Status::Passed,
            ),
        ] {
            let s = Scratch::new("exploration-finish");
            write_json(
                &s.join("replay.json"),
                &json!({"started": now(), "corpus": {"sha256": []}}),
            )
            .unwrap();
            fs::write(s.join("fuzzer.log"), log).unwrap();
            let verdict = finish(s.path(), &s.join("corpus"), Exit::SUCCESS).unwrap();
            assert_eq!(verdict.status, expected, "{log}");
            assert_eq!(
                verdict.reason.is_none(),
                expected == Status::Passed,
                "{log}"
            );
            let report: Value = read_json(&s.join("report.json")).unwrap();
            assert_eq!(
                report["status"],
                serde_json::to_value(expected).unwrap(),
                "{log}"
            );
        }
    }

    #[test]
    fn empty_and_missing_calibration_seeds_fail_explicitly() {
        for exists in [false, true] {
            let s = Scratch::new("exploration-calibration");
            let seeds = s.join("seeds");
            if exists {
                fs::create_dir(&seeds).unwrap();
            }
            let report = s.join("report");
            fs::create_dir(&report).unwrap();
            let error = calibrate(
                &report,
                &seeds,
                &s.join("unused-binary"),
                1,
                &Tools::default(),
            )
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::NotFound);
            let data: Value = read_json(&report.join("calibration.json")).unwrap();
            assert_eq!(data["status"], "failed");
            assert_eq!(data["seeds"], json!([]));
        }
    }

    #[test]
    fn begin_refuses_previous_evidence_without_changing_it() {
        let s = Scratch::new("exploration-begin");
        fs::write(s.join("report.json"), "{\"status\":\"passed\"}").unwrap();
        let campaign = Campaign {
            target: "status",
            features: Build::Default,
            seconds: 1,
            timeout: 1,
            max_len: 10,
            seed: 1,
        };
        let error = begin(
            s.path(),
            s.path(),
            &s.join("corpus"),
            &s.join("seeds"),
            &campaign,
            &Tools::default(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read_to_string(s.join("report.json")).unwrap(),
            "{\"status\":\"passed\"}"
        );
    }

    #[test]
    fn sync_seeds_keeps_discoveries_and_refreshes_changed_reviewed_contents() {
        let s = Scratch::new("exploration-sync");
        let (seeds, corpus_dir) = (s.join("seeds"), s.join("corpus"));
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
if [ "$1 $2" = "fuzz --version" ] && [ "$failure" = no-cargo-fuzz ]; then
  echo "no such command: fuzz" >&2; exit 101
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
                let root = scratch.path();
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
            fn campaign(&self, failure: Option<&str>) -> Exit {
                let mut tools = self.tools.clone();
                if let Some(failure) = failure {
                    tools
                        .vars
                        .insert("ENGINE_TEST_FAILURE".into(), failure.into());
                }
                fuzz(self.scratch.path(), &["status", "1"], &tools).unwrap()
            }
            fn reports(&self) -> Vec<PathBuf> {
                let dir = self
                    .scratch
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
            let repo = Repo::new("exploration-independent");
            assert_eq!(repo.campaign(None), Exit::SUCCESS);
            let original = repo.reports()[0].clone();
            let content = fs::read(&original).unwrap();
            assert_eq!(repo.campaign(Some("build")), Exit::new(19));
            assert_eq!(repo.reports().len(), 2);
            assert_eq!(fs::read(&original).unwrap(), content);
            let other = repo.reports().into_iter().find(|p| *p != original).unwrap();
            let other: Value = read_json(&other).unwrap();
            assert_eq!(
                (other["stage"].as_str(), other["status"].as_str()),
                (Some("build"), Some("failed"))
            );
        }

        #[test]
        fn a_prebuilt_musl_fuzzer_uses_the_rustc_host_for_build_and_run() {
            let mut repo = Repo::new("exploration-musl");
            repo.tools
                .vars
                .insert("ENGINE_TEST_MUSL_DEFAULT".into(), "1".into());
            assert_eq!(repo.campaign(None), Exit::SUCCESS);
            let data: Value = read_json(&repo.reports()[0]).unwrap();
            assert_eq!(data["status"], "passed");
            assert_eq!(data["exploration"]["executions_after_initialization"], 47);
        }

        #[test]
        fn metadata_calibration_and_exploration_failures_are_terminal() {
            let repo = Repo::new("exploration-terminal");
            for (stage, code) in [("metadata", 17), ("calibration", 1), ("exploration", 41)] {
                assert_eq!(repo.campaign(Some(stage)), Exit::new(code), "{stage}");
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

        #[test]
        fn a_missing_cargo_fuzz_fails_the_replay_settings_stage() {
            let repo = Repo::new("exploration-no-cargo-fuzz");
            assert_eq!(repo.campaign(Some("no-cargo-fuzz")), Exit::FAILURE);
            let data: Value = read_json(&repo.reports()[0]).unwrap();
            assert_eq!(
                (data["stage"].as_str(), data["status"].as_str()),
                (Some("replay-settings"), Some("failed"))
            );
            let replay = repo.reports()[0].with_file_name("replay.json");
            assert!(
                !replay.exists(),
                "no replay settings without every tool's version"
            );
        }

        #[test]
        fn a_checkout_without_git_is_an_error_before_any_evidence_is_written() {
            let repo = Repo::new("exploration-no-git");
            fs::write(repo.scratch.join("bin/git"), "#!/bin/sh\nexit 128\n").unwrap();
            let error = fuzz(repo.scratch.path(), &["status", "1"], &repo.tools).unwrap_err();
            assert!(error.to_string().contains("git rev-parse"), "{error}");
            assert!(!repo.scratch.join("target").exists());
        }
    }
}
