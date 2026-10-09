//! `cargo xtask test-timings`: how long each test takes, recorded in a
//! SQLite database to see where test time goes.
//!
//! `run` runs the suites (all of them unless `--suite` picks some) and loads
//! their `JUnit` reports; `load` loads reports from a run made some other
//! way, e.g. CI's target/nextest/ci/junit.xml. A label is `rust` for a
//! cargo-nextest report, or `browser:<config>` for a Playwright one.
//! `report` summarises the latest run of each suite.
//!
//! Suites run:
//!   rust     cargo nextest run --workspace --exclude xtask --profile ci
//!            --features monokulo/snp, as CI runs it.
//!   browser  The offline Playwright run, coverage-browser (its fixture
//!            and real-binaries projects, without instrumenting, so no
//!            coverage overhead). Needs `npm ci` in e2e/browser and
//!            crates/monokulo/pos-ui.
//!
//! Not run: the #[ignore]d stagenet/Tor Rust tests and the stagenet POS
//! Playwright suite, since they spend stagenet funds and need public nodes.
//! Their reports can still be loaded with `load`.
//!
//! Each run is a row in `runs`; each test in it a row in `tests` (see
//! `SCHEMA`). Durations come from each test's own process (nextest) or worker
//! (Playwright); tests run side by side, so they add up to more than the
//! run's wall time, which `runs.wall_s` records.

use crate::stress::target_dir;
use crate::support::{at, root, Exit};
use rusqlite::{params, types::ValueRef, Connection};
use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

pub(crate) const HELP: &str = "\
        test-timings [--db PATH] run [--suite rust|browser]...\n\
        test-timings [--db PATH] load [--note TEXT] LABEL=JUNIT...\n\
        test-timings [--db PATH] report [--top N]\n\
                      Record how long each test takes in SQLite (target/test-timings.sqlite):\n\
                      run the suites, load JUnit reports (LABEL is rust or browser:<config>),\n\
                      or summarise the latest run of each suite (xtask/src/timings.rs)";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS runs (
  id INTEGER PRIMARY KEY,
  started_at TEXT NOT NULL,
  git_commit TEXT,
  git_dirty INTEGER,
  host TEXT,
  cpus INTEGER,
  suite TEXT NOT NULL,
  command TEXT,
  wall_s REAL,
  exit_code INTEGER,
  note TEXT
);
CREATE TABLE IF NOT EXISTS tests (
  run_id INTEGER NOT NULL REFERENCES runs(id),
  crate TEXT NOT NULL,
  test_type TEXT NOT NULL CHECK (test_type IN ('unit', 'integration', 'e2e')),
  binary TEXT NOT NULL,
  test_name TEXT NOT NULL,
  duration_s REAL NOT NULL,
  status TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS tests_by_run ON tests(run_id);
CREATE VIEW IF NOT EXISTS latest AS
  SELECT tests.* FROM tests
  WHERE run_id IN (SELECT max(id) FROM runs GROUP BY suite);
";

/// The offline Playwright configurations `run` times.
const BROWSER_CONFIGS: [&str; 1] = ["coverage-browser"];
const DEFAULT_TOP: u32 = 25;

/// The suites `run` can run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Suite {
    Rust,
    Browser,
}

/// What kind of test a row is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// A library or binary's own #[test]s.
    Unit,
    /// A tests/ or examples/ target.
    Integration,
    /// A tests/e2e_* target, or any browser test.
    E2e,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Unit => "unit",
            Kind::Integration => "integration",
            Kind::E2e => "e2e",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    Passed,
    Failed,
    Skipped,
}

impl Status {
    fn name(self) -> &'static str {
        match self {
            Status::Passed => "passed",
            Status::Failed => "failed",
            Status::Skipped => "skipped",
        }
    }
}

/// One test of a run.
#[derive(Debug, PartialEq)]
struct Row {
    krate: String,
    kind: Kind,
    /// nextest's binary id, or the Playwright config and spec file.
    binary: String,
    /// The test's path within its binary, or its Playwright title.
    name: String,
    secs: f64,
    status: Status,
}

/// Which reporter wrote a `JUnit` report, and so how its rows are named.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Label {
    Rust,
    Browser(String),
}

impl Label {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "rust" => Some(Label::Rust),
            _ => text
                .strip_prefix("browser:")
                .filter(|config| !config.is_empty())
                .map(|config| Label::Browser(config.to_string())),
        }
    }
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Label::Rust => f.write_str("rust"),
            Label::Browser(config) => write!(f, "browser:{config}"),
        }
    }
}

fn status_of(case: roxmltree::Node) -> Status {
    let has = |tag: &str| case.children().any(|c| c.has_tag_name(tag));
    if has("failure") || has("error") {
        Status::Failed
    } else if has("skipped") {
        Status::Skipped
    } else {
        Status::Passed
    }
}

/// A report's rows: `name` names each suite's binary and its kind.
fn rows(path: &Path, name: impl Fn(&str) -> (String, Kind, String)) -> io::Result<Vec<Row>> {
    let text = fs::read_to_string(path).map_err(|e| at(path, e))?;
    let doc = roxmltree::Document::parse(&text)
        .map_err(|e| at(path, io::Error::new(io::ErrorKind::InvalidData, e)))?;
    let mut rows = Vec::new();
    for suite in doc.descendants().filter(|n| n.has_tag_name("testsuite")) {
        let (krate, kind, binary) = name(suite.attribute("name").unwrap_or(""));
        for case in suite.children().filter(|n| n.has_tag_name("testcase")) {
            rows.push(Row {
                krate: krate.clone(),
                kind,
                binary: binary.clone(),
                name: case.attribute("name").unwrap_or("").to_string(),
                secs: case
                    .attribute("time")
                    .and_then(|t| t.parse::<f64>().ok())
                    .filter(|s| s.is_finite() && *s >= 0.0)
                    .unwrap_or(0.0),
                status: status_of(case),
            });
        }
    }
    Ok(rows)
}

/// nextest names each testsuite after its binary id: `crate` for the
/// library, `crate::bin/name`, `crate::test_target`, `crate::example/name`.
fn rust_binary(binary: &str) -> (String, Kind, String) {
    let (krate, target) = binary.split_once("::").unwrap_or((binary, ""));
    let kind = if target.is_empty() || target.starts_with("bin/") {
        Kind::Unit
    } else if target.starts_with("e2e") {
        Kind::E2e
    } else {
        Kind::Integration
    };
    (krate.to_string(), kind, binary.to_string())
}

fn read_report(label: &Label, path: &Path) -> io::Result<Vec<Row>> {
    match label {
        Label::Rust => rows(path, rust_binary),
        Label::Browser(config) => rows(path, |spec| {
            ("e2e/browser".into(), Kind::E2e, format!("{config}:{spec}"))
        }),
    }
}

/// How a run was made, when xtask made it.
#[derive(Default)]
struct RunInfo {
    command: Option<String>,
    wall_s: Option<f64>,
    exit_code: Option<i32>,
    note: Option<String>,
}

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn host() -> Option<String> {
    fs::read_to_string("/etc/hostname")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
}

/// Seconds since the epoch as an ISO 8601 UTC time: `2026-10-09T07:31:02+00:00`.
fn utc(secs: u64) -> String {
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Days to a civil date (Howard Hinnant's algorithm), valid long past 2100.
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}+00:00",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

fn db_error(e: rusqlite::Error) -> io::Error {
    io::Error::other(format!("test timings database: {e}"))
}

/// Records one run and its tests, all or nothing.
fn record(db: &mut Connection, label: &Label, rows: &[Row], info: &RunInfo) -> io::Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let cpus = thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let tx = db.transaction().map_err(db_error)?;
    tx.execute(
        "INSERT INTO runs (started_at, git_commit, git_dirty, host, cpus, suite, command, wall_s, exit_code, note)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            utc(now),
            git(&["rev-parse", "HEAD"]),
            git(&["status", "--porcelain"]).map(|s| i64::from(!s.is_empty())),
            host(),
            i64::try_from(cpus).unwrap_or(i64::MAX),
            label.to_string(),
            info.command,
            info.wall_s,
            info.exit_code,
            info.note,
        ],
    )
    .map_err(db_error)?;
    let run = tx.last_insert_rowid();
    {
        let mut insert = tx
            .prepare("INSERT INTO tests VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)")
            .map_err(db_error)?;
        for row in rows {
            insert
                .execute(params![
                    run,
                    row.krate,
                    row.kind.name(),
                    row.binary,
                    row.name,
                    row.secs,
                    row.status.name()
                ])
                .map_err(db_error)?;
        }
    }
    tx.commit().map_err(db_error)?;
    let total: f64 = rows.iter().map(|r| r.secs).sum();
    let wall = info
        .wall_s
        .map_or_else(String::new, |w| format!(", {w:.0}s wall"));
    eprintln!("{label}: {} tests, {total:.0}s summed{wall}", rows.len());
    Ok(())
}

/// Runs a command, timing it; its exit code, or none when a signal ended it.
fn timed(command: &mut Command) -> io::Result<(f64, Option<i32>)> {
    eprintln!("+ {command:?}");
    let start = Instant::now();
    let status = command.status()?;
    Ok((start.elapsed().as_secs_f64(), status.code()))
}

fn command_line(program: &str, args: &[&str]) -> String {
    std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}

fn run_rust(db: &mut Connection) -> io::Result<()> {
    const ARGS: [&str; 10] = [
        "nextest",
        "run",
        "--workspace",
        "--locked",
        "--exclude",
        "xtask",
        "--profile",
        "ci",
        "--features",
        "monokulo/snp",
    ];
    let root = root();
    // Build first, so the recorded wall time is the tests' alone.
    timed(
        Command::new("cargo")
            .args(ARGS)
            .arg("--no-run")
            .current_dir(&root),
    )?;
    let (wall, code) = timed(Command::new("cargo").args(ARGS).current_dir(&root))?;
    // nextest keeps its store under the workspace's target/, whatever
    // CARGO_TARGET_DIR says.
    let junit = root.join("target/nextest/ci/junit.xml");
    let label = Label::Rust;
    let rows = read_report(&label, &junit)?;
    let info = RunInfo {
        command: Some(command_line("cargo", &ARGS)),
        wall_s: Some(wall),
        exit_code: code,
        note: None,
    };
    record(db, &label, &rows, &info)
}

fn run_browser(db: &mut Connection) -> io::Result<()> {
    let root = root();
    let out = target_dir().join("test-timings");
    fs::create_dir_all(&out).map_err(|e| at(&out, e))?;
    // Every binary the suites run, with the command the Playwright run's
    // global setup repeats (real-stack.js BUILD_ARGS), so their build stays
    // out of the times.
    timed(
        Command::new("node")
            .args(["-e", "require('./e2e/browser/real-stack').buildBinaries()"])
            .current_dir(&root),
    )?;
    let browser = root.join("e2e/browser");
    for config in BROWSER_CONFIGS {
        let junit = out.join(format!("browser-{config}.xml"));
        if junit.exists() {
            fs::remove_file(&junit).map_err(|e| at(&junit, e))?;
        }
        let file = format!("{config}.config.js");
        let args = ["test", "-c", file.as_str(), "--reporter=list,junit"];
        let (wall, code) = timed(
            Command::new("./node_modules/.bin/playwright")
                .args(args)
                .current_dir(&browser)
                .env("PLAYWRIGHT_JUNIT_OUTPUT_FILE", &junit),
        )?;
        let label = Label::Browser(config.to_string());
        if !junit.exists() {
            eprintln!("{label}: no report written (exit {code:?})");
            continue;
        }
        let rows = read_report(&label, &junit)?;
        let info = RunInfo {
            command: Some(command_line("./node_modules/.bin/playwright", &args)),
            wall_s: Some(wall),
            exit_code: code,
            note: None,
        };
        record(db, &label, &rows, &info)?;
    }
    Ok(())
}

/// The summaries `report` prints; `?1` is `--top`.
const REPORTS: [(&str, &str); 5] = [
    (
        "Runs",
        "SELECT id, suite, started_at, substr(git_commit, 1, 9) AS git_commit, cpus,
                round(wall_s) AS wall_s, exit_code FROM runs
         WHERE id IN (SELECT max(id) FROM runs GROUP BY suite) ORDER BY id",
    ),
    (
        "By crate and type",
        "SELECT crate, test_type, count(*) AS tests, round(sum(duration_s)) AS total_s,
                round(avg(duration_s), 2) AS mean_s, round(max(duration_s), 1) AS max_s
         FROM latest GROUP BY crate, test_type ORDER BY total_s DESC",
    ),
    (
        "By binary",
        "SELECT binary, count(*) AS tests, round(sum(duration_s)) AS total_s, round(max(duration_s), 1) AS max_s
         FROM latest GROUP BY binary ORDER BY total_s DESC LIMIT ?1",
    ),
    (
        "Slowest tests",
        "SELECT crate, test_type, test_name, round(duration_s, 1) AS duration_s, status
         FROM latest ORDER BY duration_s DESC LIMIT ?1",
    ),
    (
        "Duration buckets",
        "SELECT CASE WHEN duration_s < 0.1 THEN 'a <0.1s' WHEN duration_s < 1 THEN 'b 0.1-1s'
                     WHEN duration_s < 5 THEN 'c 1-5s' WHEN duration_s < 30 THEN 'd 5-30s'
                     ELSE 'e >=30s' END AS bucket,
                count(*) AS tests, round(sum(duration_s)) AS total_s
         FROM latest GROUP BY bucket ORDER BY bucket",
    ),
];

fn cell(value: ValueRef) -> String {
    match value {
        ValueRef::Null => String::new(),
        ValueRef::Integer(n) => n.to_string(),
        ValueRef::Real(x) if x.fract() == 0.0 => format!("{x:.1}"),
        ValueRef::Real(x) => x.to_string(),
        ValueRef::Text(t) | ValueRef::Blob(t) => String::from_utf8_lossy(t).into_owned(),
    }
}

/// The summaries as aligned text tables.
fn report(db: &Connection, top: u32) -> io::Result<String> {
    let mut out = String::new();
    for (title, sql) in REPORTS {
        let mut statement = db.prepare(sql).map_err(db_error)?;
        let headers: Vec<String> = statement
            .column_names()
            .iter()
            .map(|c| (*c).to_string())
            .collect();
        let columns = headers.len();
        let bound = if statement.parameter_count() > 0 {
            vec![top]
        } else {
            Vec::new()
        };
        let mut cells = vec![headers];
        let mut rows = statement
            .query(rusqlite::params_from_iter(bound))
            .map_err(db_error)?;
        while let Some(row) = rows.next().map_err(db_error)? {
            let mut line = Vec::with_capacity(columns);
            for i in 0..columns {
                line.push(cell(row.get_ref(i).map_err(db_error)?));
            }
            cells.push(line);
        }
        let widths: Vec<usize> = (0..columns)
            .map(|i| {
                cells
                    .iter()
                    .map(|r| r[i].chars().count())
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        out.push_str(&format!("\n{title}\n"));
        for line in &cells {
            let padded: Vec<String> = line
                .iter()
                .zip(&widths)
                .map(|(v, w)| format!("{v:<w$}"))
                .collect();
            out.push_str(padded.join("  ").trim_end());
            out.push('\n');
        }
    }
    Ok(out)
}

/// What the command line asks for.
#[derive(Debug, PartialEq)]
enum Action {
    Run(Vec<Suite>),
    Load {
        note: Option<String>,
        reports: Vec<(Label, PathBuf)>,
    },
    Report {
        top: u32,
    },
}

fn parse(args: &[&str]) -> io::Result<(PathBuf, Action)> {
    let bad = |what: String| io::Error::new(io::ErrorKind::InvalidInput, what);
    let mut args = args;
    let mut db = target_dir().join("test-timings.sqlite");
    if let ["--db", path, rest @ ..] = args {
        db = PathBuf::from(path);
        args = rest;
    }
    let action = match args {
        ["run", options @ ..] => {
            let mut suites = Vec::new();
            for pair in options.chunks(2) {
                match pair {
                    ["--suite", "rust"] => suites.push(Suite::Rust),
                    ["--suite", "browser"] => suites.push(Suite::Browser),
                    _ => return Err(bad(format!("run takes --suite rust|browser, not {pair:?}"))),
                }
            }
            if suites.is_empty() {
                suites = vec![Suite::Rust, Suite::Browser];
            }
            Action::Run(suites)
        }
        ["load", rest @ ..] => {
            let (note, rest) = match rest {
                ["--note", note, rest @ ..] => (Some((*note).to_string()), rest),
                _ => (None, rest),
            };
            if rest.is_empty() {
                return Err(bad("load needs LABEL=JUNIT...".into()));
            }
            let reports = rest
                .iter()
                .map(|report| {
                    let (label, path) = report
                        .split_once('=')
                        .ok_or_else(|| bad(format!("{report}: expected LABEL=JUNIT")))?;
                    let label = Label::parse(label).ok_or_else(|| {
                        bad(format!(
                            "unknown label {label:?}: use rust or browser:<config>"
                        ))
                    })?;
                    Ok((label, PathBuf::from(path)))
                })
                .collect::<io::Result<_>>()?;
            Action::Load { note, reports }
        }
        ["report"] => Action::Report { top: DEFAULT_TOP },
        ["report", "--top", n] => Action::Report {
            top: n
                .parse()
                .map_err(|_| bad(format!("--top takes a number, not {n}")))?,
        },
        _ => return Err(bad(HELP.trim_start().to_string())),
    };
    Ok((db, action))
}

pub(crate) fn timings(args: &[&str]) -> io::Result<Exit> {
    let (path, action) = parse(args)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|e| at(parent, e))?;
    }
    let mut db = Connection::open(&path).map_err(db_error)?;
    db.execute_batch(SCHEMA).map_err(db_error)?;
    match action {
        Action::Report { top } => {
            print!("{}", report(&db, top)?);
            return Ok(Exit::SUCCESS);
        }
        Action::Run(suites) => {
            if suites.contains(&Suite::Rust) {
                run_rust(&mut db)?;
            }
            if suites.contains(&Suite::Browser) {
                run_browser(&mut db)?;
            }
        }
        Action::Load { note, reports } => {
            for (label, report) in reports {
                let rows = read_report(&label, &report)?;
                let info = RunInfo {
                    note: note.clone(),
                    ..RunInfo::default()
                };
                record(&mut db, &label, &rows, &info)?;
            }
        }
    }
    eprintln!("wrote {}", path.display());
    Ok(Exit::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::Scratch;

    const NEXTEST: &str = r#"<testsuites>
        <testsuite name="engine"><testcase name="scanner::tests::a" time="0.5"/></testsuite>
        <testsuite name="engine::backup_restore"><testcase name="restores" time="2.0"><failure/></testcase></testsuite>
        <testsuite name="monokulo::bin/monokulo"><testcase name="main::tests::b" time="0.1"><skipped/></testcase></testsuite>
        <testsuite name="engine::e2e_stagenet"><testcase name="pays" time="30"/></testsuite>
    </testsuites>"#;

    #[test]
    fn nextest_binaries_name_the_kind_of_test() {
        let scratch = Scratch::new("timings");
        let junit = scratch.join("junit.xml");
        fs::write(&junit, NEXTEST).unwrap();
        let rows = read_report(&Label::Rust, &junit).unwrap();
        let kinds: Vec<(&str, Kind, Status)> = rows
            .iter()
            .map(|r| (r.binary.as_str(), r.kind, r.status))
            .collect();
        assert_eq!(
            kinds,
            [
                ("engine", Kind::Unit, Status::Passed),
                ("engine::backup_restore", Kind::Integration, Status::Failed),
                ("monokulo::bin/monokulo", Kind::Unit, Status::Skipped),
                ("engine::e2e_stagenet", Kind::E2e, Status::Passed),
            ]
        );
        assert_eq!(rows[1].krate, "engine");
    }

    #[test]
    fn a_loaded_report_is_recorded_and_summarised() {
        let scratch = Scratch::new("timings-db");
        let junit = scratch.join("junit.xml");
        fs::write(&junit, NEXTEST).unwrap();
        let browser = scratch.join("browser.xml");
        fs::write(
            &browser,
            r#"<testsuites><testsuite name="checkout.spec.js"><testcase name="shows the amount" time="3.5"/></testsuite></testsuites>"#,
        )
        .unwrap();
        let db = scratch.join("t.sqlite");
        let db_arg = db.to_string_lossy().into_owned();
        let rust = format!("rust={}", junit.display());
        let page = format!("browser:coverage-browser={}", browser.display());
        timings(&["--db", &db_arg, "load", "--note", "local", &rust, &page]).unwrap();
        let conn = Connection::open(&db).unwrap();
        let (runs, tests): (i64, i64) = conn
            .query_row(
                "SELECT (SELECT count(*) FROM runs), (SELECT count(*) FROM tests)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((runs, tests), (2, 5));
        let binary: String = conn
            .query_row(
                "SELECT binary FROM tests WHERE crate = 'e2e/browser'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(binary, "coverage-browser:checkout.spec.js");
        let text = report(&conn, 2).unwrap();
        assert!(text.contains("\nSlowest tests\n"), "{text}");
        assert!(text.contains("pays"), "{text}");
        // --top 2 keeps the slowest two.
        assert!(!text.contains("main::tests::b"), "{text}");
    }

    #[test]
    fn labels_and_options_are_checked() {
        assert!(parse(&["load", "pytest=x.xml"]).is_err());
        assert!(parse(&["load"]).is_err());
        assert!(parse(&["run", "--suite", "php"]).is_err());
        assert!(parse(&["report", "--top", "many"]).is_err());
        assert_eq!(
            parse(&["run", "--suite", "browser"]).unwrap().1,
            Action::Run(vec![Suite::Browser])
        );
        assert_eq!(Label::parse("browser:"), None);
    }

    #[test]
    fn times_are_written_in_utc() {
        assert_eq!(utc(0), "1970-01-01T00:00:00+00:00");
        assert_eq!(utc(1_791_453_557), "2026-10-08T09:59:17+00:00");
        assert_eq!(utc(951_782_400), "2000-02-29T00:00:00+00:00");
    }
}
