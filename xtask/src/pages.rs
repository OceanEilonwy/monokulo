//! `cargo xtask pages build`: the quality report (GitHub Pages: /quality/)
//! from CI artifacts, `cargo xtask pages fetch`: those artifacts, downloaded
//! from main's newest runs, and `cargo xtask serve`: a built site, served to
//! look at.
//!
//! Every input of the report is optional; the page says what it has no data
//! for. It writes the page (`web/pages/quality/index.html`), its data
//! (`data.json`), the screenshot gallery (`gallery/`), the report pages the
//! data links and what those need (`reports/`) and a shields.io endpoint for
//! the coverage badge (`badge.json`). Only one engine build of the property
//! and fuzz runs is shown (ZMQ unless told otherwise): they run each build
//! separately.

use crate::coverage::{Counts, CrateCoverage};
use crate::exploration::Build;
use crate::support::{
    at, css_links, files_under, html_links, read_json, unquote, write_json, write_json_compact,
    Exit,
};
use image::{codecs::jpeg::JpegEncoder, imageops::FilterType, DynamicImage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    env, fs,
    io::{self, BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::{Component, Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

/// Crates that only exist to test the others: shown, but kept out of the
/// shipping figure and the badge.
const TEST_TOOLS: [&str; 4] = [
    "cli-wallet",
    "e2e-harness",
    "engine-test-support",
    "mock-woocommerce",
];
/// The order the gallery offers a screen's shapes in.
const SHAPES: [&str; 8] = [
    "desktop",
    "element",
    "as-is",
    "tablet-landscape",
    "tablet-portrait",
    "mobile-landscape",
    "mobile-portrait",
    "small-portrait",
];
/// Full-size screenshots as JPEG: UI text stays crisp at this quality, at
/// about a third of the PNG's size.
const FULL_QUALITY: u8 = 86;
/// Thumbnails are shown scaled down, so this keeps them a few KB each.
const THUMB_QUALITY: u8 = 62;
/// Thumbnail widths: twice the grid column they fill, for dense screens.
/// Phone shots fill a narrower column and element shots a smaller frame.
const THUMB_WIDTH: u32 = 480;
const PHONE_THUMB_WIDTH: u32 = 280;
const ELEMENT_THUMB_WIDTH: u32 = 360;
/// shields.io colours by line coverage, as the Rust ecosystem's badges band it.
const BADGE_BANDS: [(f64, &str); 3] = [(90.0, "brightgreen"), (80.0, "green"), (70.0, "yellow")];
const BADGE_BELOW_BANDS: &str = "orange";
/// Folders of the exploration artifacts that hold no reports: the corpus
/// and crash inputs run to thousands of files.
const NOT_REPORTS: [&str; 4] = ["corpus", "artifacts", "semantics", "calibration-semantics"];
/// What a build writes into `--out`, and so what a rebuild replaces there.
const OUTPUTS: [&str; 7] = [
    "index.html",
    "data.json",
    "badge.json",
    ".nojekyll",
    "assets",
    "gallery",
    "reports",
];
/// A client that sends nothing is dropped after this.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) const HELP: &str = "\
        pages build --out DIR [--coverage DIR] [--properties DIR] [--fuzz DIR] [--scale DIR]\n\
                     [--sources FILE] [--feature zmq|default] [--repo-url URL]\n\
                      Build the quality report (GitHub Pages /quality/) from CI artifacts: the joined\n\
                      coverage artifact (or target/coverage), an engine-properties artifact, a folder of\n\
                      engine-fuzz artifacts, an engine-scale-measurements artifact, and a JSON file naming\n\
                      the run behind each (docs/COVERAGE.md); replaces its own files in DIR\n\
        pages fetch DIR [--repo OWNER/NAME] [--feature zmq|default]\n\
                      Download main's newest OpenWrt site, coverage, property, fuzz and scale artifacts\n\
                      into DIR, with DIR/sources.json naming their runs (needs the gh CLI)\n\
        serve DIR [PORT]\n\
                      Serve DIR on http://127.0.0.1:PORT (8000) to look at a built report: the page\n\
                      reads data.json, which a file opened from disk can't";

/// Serves a built report from `dir` until stopped: GET and HEAD only, files
/// only, nothing outside `dir`.
pub(crate) fn serve(args: &[&str]) -> io::Result<Exit> {
    let (dir, port) = match args {
        [dir] => (PathBuf::from(dir), "8000"),
        [dir, port] => (PathBuf::from(dir), *port),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "usage: cargo xtask serve DIR [PORT]",
            ))
        }
    };
    let listener = TcpListener::bind(format!("127.0.0.1:{port}"))?;
    eprintln!(
        "serving {} on http://127.0.0.1:{port}/ (Ctrl+C to stop)",
        dir.display()
    );
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let dir = dir.clone();
        // A thread per request: a browser opens several connections at once,
        // and one that stalls must not hold the others up.
        thread::spawn(move || {
            if let Err(e) = respond(&dir, stream) {
                eprintln!("serve: {e}");
            }
        });
    }
    Ok(Exit::SUCCESS)
}

/// The file a request target names inside `dir`, or nothing when it names
/// anything else: only plain names below `dir`, so no `..`, no absolute
/// paths and no drive prefixes, however they are encoded.
fn file_for(dir: &Path, target: &str) -> Option<PathBuf> {
    let path = unquote(target.split(['?', '#']).next().unwrap_or(""));
    let mut file = dir.to_path_buf();
    for part in path.split('/').filter(|p| !p.is_empty() && *p != ".") {
        let mut components = Path::new(part).components();
        match (components.next(), components.next()) {
            (Some(Component::Normal(name)), None) => file.push(name),
            _ => return None,
        }
    }
    if file.is_dir() {
        file.push("index.html");
    }
    file.is_file().then_some(file)
}

fn content_type(file: &Path) -> &'static str {
    match file.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css",
        "js" => "text/javascript",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn respond(dir: &Path, mut stream: TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    let mut request = String::new();
    BufReader::new(&stream).read_line(&mut request)?;
    let mut words = request.split_whitespace();
    let method = words.next().unwrap_or("");
    let target = words.next().unwrap_or("/");
    let (status, body, kind, allow) = match method {
        "GET" | "HEAD" => {
            match file_for(dir, target).and_then(|f| fs::read(&f).ok().map(|b| (f, b))) {
                Some((file, body)) => ("200 OK", body, content_type(&file), ""),
                None => ("404 Not Found", b"not found".to_vec(), "text/plain", ""),
            }
        }
        _ => (
            "405 Method Not Allowed",
            b"method not allowed".to_vec(),
            "text/plain",
            "Allow: GET, HEAD\r\n",
        ),
    };
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n{allow}Connection: close\r\n\r\n", body.len())?;
    if method == "HEAD" {
        return Ok(());
    }
    stream.write_all(&body)
}

/// A manifest's figures, as every coverage component records them.
#[derive(Deserialize, Default)]
struct Manifest {
    #[serde(default)]
    lines: Counts,
    #[serde(default)]
    branches: Counts,
    #[serde(default)]
    tools: ManifestTools,
}

#[derive(Deserialize, Default)]
struct ManifestTools {
    collector: Option<String>,
}

/// Line and branch counts together, as the page shows them.
#[derive(Serialize, Clone, Copy, Default)]
struct Coverage {
    lines: Counts,
    branches: Counts,
}

impl Coverage {
    fn add(&mut self, other: Coverage) {
        self.lines.covered += other.lines.covered;
        self.lines.total += other.lines.total;
        self.branches.covered += other.branches.covered;
        self.branches.total += other.branches.total;
    }
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum TestStatus {
    Passed,
    Failed,
    Skipped,
}

/// One test case of a `JUnit` report.
#[derive(Serialize)]
struct Test {
    class: String,
    name: String,
    secs: f64,
    status: TestStatus,
    /// Which browser suite: the fixture server or the real binaries.
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<&'static str>,
}

fn junit(path: &Path) -> io::Result<Vec<Test>> {
    let text = fs::read_to_string(path).map_err(|e| at(path, e))?;
    let doc = roxmltree::Document::parse(&text)
        .map_err(|e| at(path, io::Error::new(io::ErrorKind::InvalidData, e)))?;
    Ok(doc
        .descendants()
        .filter(|n| n.has_tag_name("testcase"))
        .map(|case| {
            let has = |tag: &str| case.children().any(|c| c.has_tag_name(tag));
            let status = if has("failure") || has("error") {
                TestStatus::Failed
            } else if has("skipped") {
                TestStatus::Skipped
            } else {
                TestStatus::Passed
            };
            let seconds: f64 = case
                .attribute("time")
                .and_then(|t| t.parse().ok())
                .unwrap_or(0.0);
            Test {
                class: case.attribute("classname").unwrap_or("").to_string(),
                name: case.attribute("name").unwrap_or("").to_string(),
                secs: (seconds * 1000.0).round() / 1000.0,
                status,
                kind: None,
            }
        })
        .collect())
}

/// One source file of a crate, its annotated page relative to the site.
#[derive(Serialize)]
struct FileRow {
    path: String,
    lines: Counts,
    branches: Counts,
    report: String,
}

#[derive(Serialize)]
struct Crate {
    name: String,
    #[serde(flatten)]
    coverage: Coverage,
    /// A test tool, kept out of the shipping figure.
    tool: bool,
    files: Vec<FileRow>,
    /// Source files no test profile instrumented, relative to the repository.
    unmeasured: Vec<String>,
}

#[derive(Serialize, Default)]
struct Tests {
    rust: Vec<Test>,
    browser: Vec<Test>,
    woocommerce: Vec<Test>,
}

/// The coverage artifact as the page shows it.
#[derive(Serialize)]
struct CoverageData {
    /// Each collector's status from run.json.
    components: BTreeMap<String, Value>,
    revision: Value,
    /// The figures of each component that left a manifest.
    totals: BTreeMap<&'static str, Coverage>,
    tools: BTreeMap<&'static str, Option<String>>,
    crates: Vec<Crate>,
    tests: Tests,
    /// The report pages shipped under reports/, by component.
    reports: BTreeMap<&'static str, String>,
    /// The Rust crates that ship, added up: the headline figure and the badge.
    shipping: Coverage,
}

/// The artifact files the page links, and so the roots of what ships.
struct Linked(BTreeSet<String>);

/// Every suite's results from the `JUnit` reports the collectors left.
fn test_results(src: &Path) -> io::Result<Tests> {
    let mut tests = Tests::default();
    if src.join("rust/junit.xml").is_file() {
        tests.rust = junit(&src.join("rust/junit.xml"))?;
    }
    for (file, kind) in [
        ("junit-fixture.xml", "fixture"),
        ("junit-real-binaries.xml", "real binaries"),
    ] {
        let path = src.join("browser").join(file);
        if path.is_file() {
            tests
                .browser
                .extend(junit(&path)?.into_iter().map(|t| Test {
                    kind: Some(kind),
                    ..t
                }));
        }
    }
    if src.join("woocommerce/junit.xml").is_file() {
        tests.woocommerce = junit(&src.join("woocommerce/junit.xml"))?;
    }
    Ok(tests)
}

/// Coverage totals, per-crate and per-file Rust figures, and every test result.
fn coverage(src: &Path) -> io::Result<(CoverageData, Linked)> {
    let run: Value = read_json(&src.join("run.json"))?;
    let components = run["components"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            (
                c["component"].as_str().unwrap_or("").to_string(),
                c["status"].clone(),
            )
        })
        .collect();
    let (mut totals, mut tools) = (BTreeMap::new(), BTreeMap::new());
    for name in ["rust", "browser", "woocommerce"] {
        let manifest = src.join(format!("{name}.json"));
        if manifest.is_file() {
            let m: Manifest = read_json(&manifest)?;
            totals.insert(
                name,
                Coverage {
                    lines: m.lines,
                    branches: m.branches,
                },
            );
            tools.insert(name, m.tools.collector);
        }
    }
    let mut linked = BTreeSet::new();
    let mut crates = Vec::new();
    if src.join("rust-crates.json").is_file() {
        let summaries: Vec<CrateCoverage> = read_json(&src.join("rust-crates.json"))?;
        for c in summaries {
            let files = c
                .files
                .into_iter()
                .map(|f| {
                    linked.insert(f.report.clone());
                    FileRow {
                        path: f.path,
                        lines: f.lines,
                        branches: f.branches,
                        report: format!("reports/{}", f.report),
                    }
                })
                .collect();
            crates.push(Crate {
                tool: TEST_TOOLS.contains(&c.component.as_str()),
                name: c.component,
                coverage: Coverage {
                    lines: c.lines,
                    branches: c.branches,
                },
                files,
                unmeasured: c.unavailable_files,
            });
        }
    }
    let tests = test_results(src)?;
    let mut reports = BTreeMap::new();
    for name in ["rust", "browser", "woocommerce", "stress"] {
        let index = format!("{name}/index.html");
        if src.join(&index).is_file() {
            reports.insert(name, format!("reports/{index}"));
            linked.insert(index);
        }
    }
    let mut shipping = Coverage::default();
    for c in crates.iter().filter(|c| !c.tool) {
        shipping.add(c.coverage);
    }
    Ok((
        CoverageData {
            components,
            revision: run["revision"].clone(),
            totals,
            tools,
            crates,
            tests,
            reports,
            shipping,
        },
        Linked(linked),
    ))
}

/// `a/b/../c` as `a/c`; nothing when the path climbs out of the root.
fn normalise(path: &Path) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::Normal(p) => parts.push(p.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(parts.join("/"))
}

/// Copies into `out/reports/` the artifact files the page links, and every
/// file those pages and stylesheets link in turn, so the reports work and
/// nothing unlinked ships.
fn ship_reports(src: &Path, out: &Path, linked: &Linked) -> io::Result<()> {
    let mut queue: VecDeque<String> = linked.0.iter().cloned().collect();
    let mut shipped = BTreeSet::new();
    while let Some(rel) = queue.pop_front() {
        if !shipped.insert(rel.clone()) {
            continue;
        }
        let from = src.join(&rel);
        if !from.is_file() {
            // A report may link what its generator never wrote; the page's
            // own links were checked by the artifact's validation.
            continue;
        }
        let to = out.join("reports").join(&rel);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&from, &to).map_err(|e| at(&from, e))?;
        let extension = from.extension().and_then(|e| e.to_str()).unwrap_or("");
        let links = match extension {
            "html" => html_links(&fs::read_to_string(&from).map_err(|e| at(&from, e))?),
            "css" => css_links(&fs::read_to_string(&from).map_err(|e| at(&from, e))?),
            _ => continue,
        };
        let dir = Path::new(&rel).parent().unwrap_or(Path::new(""));
        queue.extend(links.iter().filter_map(|link| normalise(&dir.join(link))));
    }
    Ok(())
}

/// One scan round of a stress point: how long, and how far behind it left stores.
#[derive(Serialize)]
struct Tick {
    phase: String,
    ms: u64,
    lagging: u64,
    lag: u64,
}

/// The one-CPU stress run (CI) or the weekly scale run, as the page draws them.
fn stress(run_file: &Path) -> io::Result<Value> {
    let run: Value = read_json(run_file)?;
    let scenario = &run["scenario"];
    let point = |result: &Value, name: Value| -> Value {
        let fx = &result["fixture"];
        let points = fx["points"].as_array().map_or(&[][..], Vec::as_slice);
        let peak = result["peak_resident_bytes"].as_u64().unwrap_or_else(|| {
            points
                .iter()
                .filter_map(|t| t["process_memory"]["peak_resident_bytes"].as_u64())
                .max()
                .unwrap_or(0)
        });
        let tenants = if result["tenants"].is_null() {
            &fx["tenants"]
        } else {
            &result["tenants"]
        };
        let ticks: Vec<Tick> = points
            .iter()
            .map(|t| Tick {
                phase: t["phase"].as_str().unwrap_or("").to_string(),
                ms: t["duration_ms"].as_u64().unwrap_or(0),
                lagging: t["lagging_tenants"].as_u64().unwrap_or(0),
                lag: t["oldest_lag_blocks"].as_u64().unwrap_or(0),
            })
            .collect();
        json!({
            "name": name, "status": result["status"], "detail": result["detail"], "tenants": tenants, "ticks": ticks,
            "peak_mb": if peak > 0 { json!((peak as f64 / 1_048_576.0 * 10.0).round() / 10.0) } else { Value::Null },
            "http_max_us": fx["http_max_latency_us"], "db_write_max_us": fx["db_write_query_max_us"],
            "rpc_calls": fx["rpc_calls"], "rpc_failures": fx["rpc_failures"], "rpc_delay_ms": fx["rpc_delay_ms"],
            "rpc_fail_every": fx["rpc_fail_every"], "rpc_fail_until_height": fx["rpc_fail_until_height"],
            "custody_slots": fx["custody_slots"], "custody_delay_ms": fx["custody_delay_ms"],
            "custody_scans": fx["custody_scans_completed"], "custody_max_wait_us": fx["custody_max_wait_us"],
            "lock_ms": fx["write_lock_hold_ms_per_tick"], "measured_ticks": fx["measured_ticks"], "drain_ticks": fx["drain_ticks"],
        })
    };
    let hardware = match &run["hardware"] {
        Value::Object(inline) => Value::Object(inline.clone()),
        // The run names the file it wrote the hardware to, beside itself.
        Value::String(file) => {
            let beside = run_file.with_file_name(file);
            if beside.is_file() {
                read_json(&beside)?
            } else {
                json!({})
            }
        }
        _ => json!({}),
    };
    let list = |key: &str, name: &dyn Fn(&Value) -> Value| -> Vec<Value> {
        run[key]
            .as_array()
            .into_iter()
            .flatten()
            .map(|r| point(r, name(r)))
            .collect()
    };
    Ok(json!({
        "profile": run["profile"], "started": run["started_at_utc"],
        "budget_ms": scenario["poll_interval_ms"], "max_lag_blocks": scenario["max_oldest_lag_blocks"],
        "max_http_ms": scenario["max_http_latency_ms"], "fault_lock_ms": scenario["fault_sqlite_lock_ms"],
        "points": list("results", &|r: &Value| format!("point-{}", r["tenants"]).into()),
        "faults": list("faults", &|r: &Value| r["file"].clone()),
        "hardware": {
            "cpu_model": hardware["cpu_model"], "effective_cores": hardware["effective_cores"],
            "sqlite_version": hardware["sqlite_version"], "selected_cpu": hardware["selected_cpu"],
        },
    }))
}

/// The report files of an exploration artifact, leaving its corpora alone.
fn report_files(src: &Path) -> io::Result<Vec<PathBuf>> {
    files_under(src, |dir| {
        dir.file_name()
            .is_some_and(|n| NOT_REPORTS.iter().any(|skip| n == *skip))
    })
}

/// The nightly property run of one build: its settings, scenario counts and
/// every property, from the `JUnit` report beside its `report.json`.
fn properties(src: &Path, build: Build) -> io::Result<Value> {
    for path in report_files(src)?
        .iter()
        .filter(|p| p.ends_with("engine-exploration/properties/report.json"))
    {
        let report: Value = read_json(path)?;
        if report["settings"]["ENGINE_FEATURES"].as_str() != Some(build.features()) {
            continue;
        }
        // .../target/engine-exploration/properties/report.json and
        // .../target/nextest/ci/junit.xml come from the same run.
        let junit_file = path
            .ancestors()
            .nth(3)
            .map(|target| target.join("nextest/ci/junit.xml"))
            .filter(|p| p.is_file());
        let tests = match junit_file {
            Some(path) => junit(&path)?,
            None => Vec::new(),
        };
        return Ok(json!({
            "revision": report["revision"], "settings": report["settings"], "cases": report["semantic_cases"],
            "observations": report["semantic_observations"], "tests": tests,
        }));
    }
    Ok(Value::Null)
}

/// Last night's campaign for each fuzz target of one build, deepest first.
fn fuzz(src: &Path, build: Build) -> io::Result<Value> {
    let mut targets: BTreeMap<String, Value> = BTreeMap::new();
    for path in report_files(src)?
        .iter()
        .filter(|p| p.ends_with("report.json"))
    {
        // .../engine-exploration/fuzz/<target>/<build>/<seed>/<id>/report.json
        let parts: Vec<String> = path
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let n = parts.len();
        if n < 7
            || parts[n - 6] != "fuzz"
            || parts[n - 7] != "engine-exploration"
            || parts[n - 4] != build.name()
        {
            continue;
        }
        let target = parts[n - 5].clone();
        let r: Value = read_json(path)?;
        let ex = &r["exploration"];
        targets.insert(
            target.clone(),
            json!({
                "target": target, "status": r["status"], "seconds": r["wall_seconds"].as_f64().unwrap_or(0.0).round(),
                "corpus": r["corpus"]["files"], "new_inputs": r["new_unique_inputs"],
                "edges": ex["final"]["coverage"], "edge_growth": ex["coverage_growth"], "execs": ex["final"]["executions"],
                "cases": r["semantic_cases"],
                "observations": if r["semantic_observations"].is_object() { r["semantic_observations"].clone() } else { json!({}) },
            }),
        );
    }
    let mut list: Vec<Value> = targets.into_values().collect();
    list.sort_by_key(|t| std::cmp::Reverse(t["edges"].as_u64().unwrap_or(0)));
    Ok(if list.is_empty() {
        Value::Null
    } else {
        list.into()
    })
}

fn save_jpeg(image: &DynamicImage, path: &Path, quality: u8) -> io::Result<()> {
    let mut file = io::BufWriter::new(fs::File::create(path).map_err(|e| at(path, e))?);
    image
        .to_rgb8()
        .write_with_encoder(JpegEncoder::new_with_quality(&mut file, quality))
        .map_err(|e| at(path, io::Error::other(e)))?;
    file.flush()
}

/// A screenshot as the grid's thumbnail and the viewer's full image.
#[derive(Serialize, Clone)]
struct Image {
    src: String,
    w: u32,
    h: u32,
    full: String,
    fw: u32,
    fh: u32,
}

/// One screenshot as the viewer's full image and the grid's thumbnail.
fn convert(image: &Path, shape: &str, name: &str, out: &Path) -> io::Result<Image> {
    let full = image::open(image).map_err(|e| at(image, io::Error::other(e)))?;
    save_jpeg(&full, &out.join("gallery/full").join(name), FULL_QUALITY)?;
    let width = match shape {
        "mobile-portrait" | "small-portrait" => PHONE_THUMB_WIDTH,
        "element" => ELEMENT_THUMB_WIDTH,
        _ => THUMB_WIDTH,
    }
    .min(full.width());
    let height = u64::from(full.height()) * u64::from(width) / u64::from(full.width().max(1));
    let height = u32::try_from(height.max(1)).unwrap_or(u32::MAX);
    let thumb = full.resize_exact(width, height, FilterType::CatmullRom);
    save_jpeg(&thumb, &out.join("gallery/thumb").join(name), THUMB_QUALITY)?;
    Ok(Image {
        src: format!("gallery/thumb/{name}"),
        w: thumb.width(),
        h: thumb.height(),
        full: format!("gallery/full/{name}"),
        fw: full.width(),
        fh: full.height(),
    })
}

/// One screenshot of the browser tests' manifest, as the collector wrote it.
#[derive(Deserialize)]
struct Shot {
    group: String,
    stage: String,
    #[serde(default)]
    test: Option<String>,
    #[serde(default)]
    status: Option<String>,
    /// The Playwright report the test is in, relative to the screenshots folder.
    #[serde(default)]
    report: Option<String>,
    shape: String,
    theme: String,
    image: String,
    #[serde(default)]
    retry: u64,
}

/// A screen the browser tests photographed: one stage of one test group,
/// in every shape and theme it was taken in.
#[derive(Serialize)]
struct Screen {
    group: String,
    stage: String,
    test: Option<String>,
    status: Option<String>,
    /// The Playwright report the test is in, relative to the site.
    report: Option<String>,
    /// By shape, then theme.
    images: BTreeMap<String, BTreeMap<String, Image>>,
    /// Screenshots taken, retries left out.
    count: u64,
    /// The shapes in the gallery's order.
    shapes: Vec<String>,
}

#[derive(Serialize)]
struct Gallery {
    stages: Vec<Screen>,
    total: usize,
}

/// A screenshot waiting to be converted, and where its images go.
struct Conversion {
    screen: usize,
    shape: String,
    theme: String,
    image: PathBuf,
    name: String,
}

/// Every screenshot as a thumbnail for the grid and the full image for the
/// viewer; the Playwright reports the screens link.
fn gallery(src: &Path, out: &Path) -> io::Result<Option<(Gallery, Linked)>> {
    let manifest = src.join("screenshots/manifest.json");
    if !manifest.is_file() {
        return Ok(None);
    }
    let shots: Vec<Shot> = read_json(&manifest)?;
    let shots: Vec<&Shot> = shots.iter().filter(|s| s.retry == 0).collect();
    fs::create_dir_all(out.join("gallery/full"))?;
    fs::create_dir_all(out.join("gallery/thumb"))?;
    let mut screens: Vec<Screen> = Vec::new();
    let mut index: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    let mut linked = BTreeSet::new();
    // The first shot of each screen's shape and theme is the one shown.
    let mut seen: BTreeSet<(usize, &str, &str)> = BTreeSet::new();
    let mut jobs: Vec<Conversion> = Vec::new();
    for shot in &shots {
        let i = *index.entry((&shot.group, &shot.stage)).or_insert_with(|| {
            let report = shot.report.as_deref().and_then(|r| r.strip_prefix("../"));
            if let Some(report) = report {
                linked.insert(report.to_string());
            }
            screens.push(Screen {
                group: shot.group.clone(),
                stage: shot.stage.clone(),
                test: shot.test.clone(),
                status: shot.status.clone(),
                report: report.map(|r| format!("reports/{r}")),
                images: BTreeMap::new(),
                count: 0,
                shapes: Vec::new(),
            });
            screens.len() - 1
        });
        let screen = &mut screens[i];
        screen.count += 1;
        if shot.status.as_deref() != Some("passed") {
            screen.status.clone_from(&shot.status);
        }
        if !seen.insert((i, &shot.shape, &shot.theme)) {
            continue;
        }
        jobs.push(Conversion {
            screen: i,
            image: src.join("screenshots").join(&shot.image),
            // Stages repeat across groups, so the group is part of the name.
            name: format!(
                "{}-{}-{}-{}.jpg",
                shot.group, shot.stage, shot.shape, shot.theme
            ),
            shape: shot.shape.clone(),
            theme: shot.theme.clone(),
        });
    }
    // Each screenshot converts on its own, so they share out over every core:
    // a CI run has hundreds.
    let threads = thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let converted: Vec<io::Result<Image>> = thread::scope(|scope| {
        let chunk = jobs.len().div_ceil(threads).max(1);
        let handles: Vec<_> = jobs
            .chunks(chunk)
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|job| convert(&job.image, &job.shape, &job.name, out))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a gallery thread panicked"))
            .collect()
    });
    for (job, image) in jobs.iter().zip(converted) {
        screens[job.screen]
            .images
            .entry(job.shape.clone())
            .or_default()
            .insert(job.theme.clone(), image?);
    }
    for screen in &mut screens {
        let mut shapes: Vec<String> = screen.images.keys().cloned().collect();
        shapes.sort_by_key(|s| SHAPES.iter().position(|k| k == s).unwrap_or(SHAPES.len()));
        screen.shapes = shapes;
    }
    screens.sort_by(|a, b| (&a.group, &a.stage).cmp(&(&b.group, &b.stage)));
    Ok(Some((
        Gallery {
            stages: screens,
            total: shots.len(),
        },
        Linked(linked),
    )))
}

fn badge(shipping: Coverage) -> Value {
    let (covered, total) = (shipping.lines.covered, shipping.lines.total);
    if total == 0 {
        return json!({"schemaVersion": 1, "label": "coverage", "message": "unknown", "color": "lightgrey"});
    }
    let pct = 100.0 * covered as f64 / total as f64;
    let color = BADGE_BANDS
        .iter()
        .find(|(floor, _)| pct >= *floor)
        .map_or(BADGE_BELOW_BANDS, |(_, colour)| colour);
    json!({"schemaVersion": 1, "label": "coverage", "message": format!("{pct:.1}%"), "color": color})
}

struct SiteArgs {
    out: Option<PathBuf>,
    coverage: Option<PathBuf>,
    properties: Option<PathBuf>,
    fuzz: Option<PathBuf>,
    scale: Option<PathBuf>,
    sources: Option<PathBuf>,
    build: Build,
    repo_url: String,
}

fn parse(args: &[&str]) -> io::Result<SiteArgs> {
    let mut parsed = SiteArgs {
        out: None,
        coverage: None,
        properties: None,
        fuzz: None,
        scale: None,
        sources: None,
        build: Build::Zmq,
        repo_url: "https://github.com/OceanEilonwy/monokulo".into(),
    };
    let bad = |what: String| io::Error::new(io::ErrorKind::InvalidInput, what);
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let value = rest
            .next()
            .ok_or_else(|| bad(format!("{flag} needs a value")))?;
        let path = Some(PathBuf::from(value));
        match *flag {
            "--out" => parsed.out = path,
            "--coverage" => parsed.coverage = path,
            "--properties" => parsed.properties = path,
            "--fuzz" => parsed.fuzz = path,
            "--scale" => parsed.scale = path,
            "--sources" => parsed.sources = path,
            "--feature" => {
                parsed.build =
                    Build::parse(value).ok_or_else(|| bad("--feature is zmq or default".into()))?;
            }
            "--repo-url" => parsed.repo_url = value.to_string(),
            _ => return Err(bad(format!("unknown option {flag}"))),
        }
    }
    Ok(parsed)
}

/// Everything the page reads, written to data.json.
#[derive(Serialize)]
struct Data {
    feature: Build,
    repo: String,
    sources: Value,
    coverage: Option<CoverageData>,
    stress: Option<Value>,
    gallery: Option<Gallery>,
    properties: Value,
    fuzz: Value,
    scale: Option<Value>,
}

/// Builds the report into `--out`, replacing what an earlier build put there
/// and nothing else.
pub(crate) fn build(root: &Path, args: &[&str]) -> io::Result<Exit> {
    let args = parse(args)?;
    let out = args.out.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "pages build needs --out DIR")
    })?;
    for name in OUTPUTS {
        let path = out.join(name);
        if path.is_dir() {
            fs::remove_dir_all(&path).map_err(|e| at(&path, e))?;
        } else if path.exists() {
            fs::remove_file(&path).map_err(|e| at(&path, e))?;
        }
    }
    fs::create_dir_all(&out).map_err(|e| at(&out, e))?;
    let mut data = Data {
        feature: args.build,
        repo: args.repo_url.clone(),
        sources: match &args.sources {
            Some(path) => read_json(path)?,
            None => json!({}),
        },
        coverage: None,
        stress: None,
        gallery: None,
        properties: Value::Null,
        fuzz: Value::Null,
        scale: None,
    };
    if let Some(src) = &args.coverage {
        let (coverage, mut linked) = coverage(src)?;
        data.coverage = Some(coverage);
        let run = src.join("stress/run.json");
        if run.is_file() {
            data.stress = Some(stress(&run)?);
        }
        if let Some((gallery, reports)) = gallery(src, &out)? {
            data.gallery = Some(gallery);
            linked.0.extend(reports.0);
        }
        ship_reports(src, &out, &linked)?;
    }
    if let Some(src) = &args.properties {
        data.properties = properties(src, args.build)?;
    }
    if let Some(src) = &args.fuzz {
        data.fuzz = fuzz(src, args.build)?;
    }
    if let Some(src) = &args.scale {
        if let Some(run) = report_files(src)?
            .into_iter()
            .find(|p| p.ends_with("run.json"))
        {
            data.scale = Some(stress(&run)?);
        }
    }
    write_json_compact(&out.join("data.json"), &data)?;
    let shipping = data
        .coverage
        .as_ref()
        .map_or_else(Coverage::default, |c| c.shipping);
    write_json(&out.join("badge.json"), &badge(shipping))?;
    fs::create_dir_all(out.join("assets"))?;
    let theme = root.join("crates/monokulo/src/views/theme.css");
    fs::copy(&theme, out.join("assets/theme.css")).map_err(|e| at(&theme, e))?;
    for weight in [500, 700, 800] {
        let font = root
            .join("crates/monokulo/static")
            .join(format!("manrope-{weight}.woff2"));
        fs::copy(
            &font,
            out.join("assets").join(format!("manrope-{weight}.woff2")),
        )
        .map_err(|e| at(&font, e))?;
    }
    let page = root.join("web/pages/quality/index.html");
    let page = fs::read_to_string(&page).map_err(|e| at(&page, e))?;
    fs::write(
        out.join("index.html"),
        page.replace("@REPO_URL@", &args.repo_url),
    )?;
    fs::write(out.join(".nojekyll"), "")?;
    let have: Vec<&str> = [
        ("coverage", data.coverage.is_some()),
        ("properties", !data.properties.is_null()),
        ("fuzz", !data.fuzz.is_null()),
        ("scale", data.scale.is_some()),
    ]
    .into_iter()
    .filter_map(|(name, present)| present.then_some(name))
    .collect();
    eprintln!(
        "quality report in {}: {}",
        out.display(),
        if have.is_empty() {
            "no data".into()
        } else {
            have.join(", ")
        }
    );
    Ok(Exit::SUCCESS)
}

/// One of the artifacts the Pages site is built from.
struct Source {
    key: &'static str,
    workflow: &'static str,
    /// Whether an artifact name is this source's, for the build shown.
    wanted: fn(&str, Build) -> bool,
    /// The site is required because Pages serves one deployment, so
    /// publishing without it would take the package repository offline.
    required: bool,
    /// Only a run that passed will do (a failed run's results are shown, not hidden, otherwise).
    needs_success: bool,
}

const SOURCES: [Source; 5] = [
    Source {
        key: "site",
        workflow: "openwrt.yml",
        wanted: |n, _| n == "monokulo-openwrt-site",
        required: true,
        needs_success: true,
    },
    Source {
        key: "coverage",
        workflow: "ci.yml",
        wanted: |n, _| {
            n.strip_prefix("coverage-")
                .is_some_and(|sha| sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
        },
        required: false,
        needs_success: false,
    },
    Source {
        key: "properties",
        workflow: "engine-properties.yml",
        wanted: |n, build| n.starts_with(&format!("engine-properties-{build}-")),
        required: false,
        needs_success: false,
    },
    Source {
        key: "fuzz",
        workflow: "engine-fuzz.yml",
        wanted: |n, build| {
            n.starts_with("engine-fuzz-") && n.rsplit('-').nth(1) == Some(build.name())
        },
        required: false,
        needs_success: false,
    },
    Source {
        key: "scale",
        workflow: "engine-scale.yml",
        wanted: |n, _| n.starts_with("engine-scale-measurements-"),
        required: false,
        needs_success: false,
    },
];
const RUNS_TO_SEARCH: u32 = 30;

fn gh(args: &[&str]) -> io::Result<String> {
    let output = Command::new("gh")
        .args(args)
        .output()
        .map_err(|e| io::Error::new(e.kind(), format!("missing prerequisite: gh ({e})")))?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "gh {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn api(repo: &str, path: &str) -> io::Result<Value> {
    serde_json::from_str(&gh(&["api", &format!("repos/{repo}/{path}")])?)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("gh api {path}: {e}")))
}

struct FetchArgs {
    dir: PathBuf,
    repo: String,
    build: Build,
}

fn parse_fetch(args: &[&str]) -> io::Result<FetchArgs> {
    let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidInput, what.to_string());
    let [dir, options @ ..] = args else {
        return Err(bad(
            "usage: cargo xtask pages fetch DIR [--repo OWNER/NAME] [--feature zmq|default]",
        ));
    };
    let mut repo = env::var("GITHUB_REPOSITORY").ok();
    let mut build = Build::Zmq;
    let mut rest = options.iter();
    while let Some(flag) = rest.next() {
        let value = rest
            .next()
            .ok_or_else(|| bad(&format!("{flag} needs a value")))?;
        match *flag {
            "--repo" => repo = Some((*value).to_string()),
            "--feature" => {
                build = Build::parse(value).ok_or_else(|| bad("--feature is zmq or default"))?;
            }
            _ => return Err(bad(&format!("unknown option {flag}"))),
        }
    }
    Ok(FetchArgs {
        dir: PathBuf::from(dir),
        repo: repo.ok_or_else(|| bad("name the repository with --repo or GITHUB_REPOSITORY"))?,
        build,
    })
}

/// Downloads main's newest artifacts of each source into `dir`. For each it
/// takes the newest finished run that still has them, skipping cancelled
/// runs, and writes `dir/sources.json` naming the run behind each.
pub(crate) fn fetch(args: &[&str]) -> io::Result<Exit> {
    let FetchArgs { dir, repo, build } = parse_fetch(args)?;
    let mut sources = Map::new();
    for source in &SOURCES {
        let (key, workflow) = (source.key, source.workflow);
        let runs = api(&repo, &format!("actions/workflows/{workflow}/runs?branch=main&status=completed&per_page={RUNS_TO_SEARCH}"))?;
        let mut found = None;
        for run in runs["workflow_runs"].as_array().into_iter().flatten() {
            let conclusion = run["conclusion"].as_str().unwrap_or("");
            if conclusion == "cancelled"
                || conclusion == "skipped"
                || (source.needs_success && conclusion != "success")
            {
                continue;
            }
            let artifacts = api(
                &repo,
                &format!("actions/runs/{}/artifacts?per_page=100", run["id"]),
            )?;
            let names: Vec<String> = artifacts["artifacts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|a| a["expired"] == false)
                .filter_map(|a| a["name"].as_str())
                .filter(|n| (source.wanted)(n, build))
                .map(str::to_string)
                .collect();
            if !names.is_empty() {
                found = Some((run.clone(), names));
                break;
            }
        }
        let Some((run, names)) = found else {
            if source.required {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "{key}: no run of {workflow} on main in the last {RUNS_TO_SEARCH} still has its artifact; \
                         run that workflow on main (Actions > Run workflow), then this one again"
                    ),
                ));
            }
            eprintln!("{key}: no artifacts found, skipped");
            continue;
        };
        let id = run["id"].to_string();
        for name in &names {
            // One artifact goes straight in; several (the fuzz targets) get a folder each.
            let dest = if names.len() > 1 {
                dir.join(key).join(name)
            } else {
                dir.join(key)
            };
            gh(&[
                "run",
                "download",
                &id,
                "--repo",
                &repo,
                "--name",
                name,
                "--dir",
                &dest.to_string_lossy(),
            ])?;
        }
        eprintln!(
            "{key}: run {id} ({}, {}), {} artifact(s)",
            run["conclusion"].as_str().unwrap_or("?"),
            run["updated_at"].as_str().unwrap_or("?"),
            names.len()
        );
        sources.insert(
            key.into(),
            json!({"run_id": run["id"], "sha": run["head_sha"], "date": run["updated_at"], "conclusion": run["conclusion"], "artifacts": names.len()}),
        );
    }
    fs::create_dir_all(&dir).map_err(|e| at(&dir, e))?;
    write_json(&dir.join("sources.json"), &sources)?;
    if let Ok(summary) = env::var("GITHUB_STEP_SUMMARY") {
        let mut out = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(summary)?;
        writeln!(
            out,
            "### Pages sources\n\n| Source | Run | Result | Finished |\n|---|---|---|---|"
        )?;
        for (key, run) in &sources {
            writeln!(
                out,
                "| {key} | [{id}](https://github.com/{repo}/actions/runs/{id}) | {} | {} |",
                run["conclusion"].as_str().unwrap_or(""),
                run["date"].as_str().unwrap_or(""),
                id = run["run_id"]
            )?;
        }
    }
    Ok(Exit::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::Scratch;
    use image::{Rgb, RgbImage};
    use std::io::Read;

    const SHA: &str = "7e8f34dac14b985bd24a323147b71a10e2bc4b05";

    fn put(path: &Path, text: impl AsRef<[u8]>) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn put_json(path: &Path, value: &Value) {
        put(path, serde_json::to_vec(value).unwrap());
    }

    fn junit_xml(cases: &[(&str, &str, f64, &str)]) -> String {
        let rows = cases
            .iter()
            .map(|(class, name, time, extra)| format!(r#"<testcase name="{name}" classname="{class}" time="{time}">{extra}</testcase>"#))
            .collect::<Vec<_>>()
            .concat();
        format!("<testsuites><testsuite>{rows}</testsuite></testsuites>")
    }

    fn counts(l: u64, lt: u64, b: u64, bt: u64) -> Value {
        json!({"lines": {"covered": l, "total": lt}, "branches": {"covered": b, "total": bt}})
    }

    fn merged(mut a: Value, b: &Value) -> Value {
        a.as_object_mut()
            .unwrap()
            .extend(b.as_object().unwrap().clone());
        a
    }

    /// A cut-down coverage-<sha> artifact: two crates, three suites, the
    /// stress run, screenshots of two groups, and the files a run leaves
    /// that no report links.
    fn coverage_artifact(root: &Path) {
        put_json(
            &root.join("run.json"),
            &json!({"revision": SHA, "components": [{"component": "rust", "status": "passed"}]}),
        );
        put_json(
            &root.join("rust.json"),
            &merged(
                counts(90, 100, 8, 10),
                &json!({"tools": {"collector": "cargo-llvm-cov 0.9.1"}}),
            ),
        );
        put_json(
            &root.join("browser.json"),
            &merged(
                counts(19, 20, 9, 10),
                &json!({"tools": {"collector": "istanbul"}}),
            ),
        );
        let scanner = "rust/coverage/home/runner/crates/engine/src/scanner.rs.html";
        put_json(
            &root.join("rust-crates.json"),
            &json!([
                merged(
                    counts(80, 90, 8, 9),
                    &json!({"component": "engine", "report": "rust/crates/engine.html", "measured_files": 1,
                        "unavailable_files": ["crates/engine/src/lib.rs"],
                        "files": [merged(counts(80, 90, 8, 9), &json!({"path": "src/scanner.rs", "report": scanner}))]})
                ),
                merged(
                    counts(10, 40, 0, 1),
                    &json!({"component": "cli-wallet", "report": "rust/crates/cli-wallet.html", "measured_files": 0,
                        "unavailable_files": [], "files": []})
                ),
            ]),
        );
        put(
            &root.join("rust/index.html"),
            r#"<link rel='stylesheet' href='style.css'><a href="coverage/home/runner/crates/engine/src/scanner.rs.html">scanner</a>"#,
        );
        put(
            &root.join(scanner),
            "<link rel='stylesheet' href='../../../../../../style.css'><script src='../../../../../../control.js'></script>",
        );
        put(&root.join("rust/style.css"), "body{}");
        put(&root.join("rust/control.js"), "");
        put(&root.join("rust/raw.json"), "{}");
        put(&root.join("rust/test.log"), "");
        put(&root.join("rust/result.json"), "{}");
        put(
            &root.join("rust/junit.xml"),
            junit_xml(&[
                (
                    "engine",
                    "scanner::tests::a_reorg_moves_the_payment",
                    0.5,
                    "",
                ),
                ("engine", "scanner::tests::a_broken_case", 0.1, "<failure/>"),
                (
                    "engine",
                    "scanner::tests::an_ignored_case",
                    0.0,
                    "<skipped/>",
                ),
            ]),
        );
        put(
            &root.join("browser/junit-fixture.xml"),
            junit_xml(&[("checkout.spec.js", "checkout shows the amount", 2.0, "")]),
        );
        put(
            &root.join("browser/index.html"),
            r#"<link rel="stylesheet" href="base.css"><img src="sort-arrow-sprite.png">"#,
        );
        put(
            &root.join("browser/base.css"),
            "th{background:url(sort-arrow-sprite.png)}",
        );
        put(&root.join("browser/sort-arrow-sprite.png"), "png");
        put(&root.join("browser/coverage-final.json"), "{}");
        put(&root.join("browser/assets/pos-app.js"), "");
        put(
            &root.join("browser/playwright-report/index.html"),
            "<html></html>",
        );
        let tick = |phase: &str, ms: u64| json!({"phase": phase, "duration_ms": ms, "lagging_tenants": 0, "oldest_lag_blocks": 0});
        let point = |tenants: u64, ms: u64| {
            json!({"tenants": tenants, "status": "sustainable", "peak_resident_bytes": 24 * 1_048_576, "fixture": {
                "tenants": tenants, "http_max_latency_us": 5000, "measured_ticks": 2, "drain_ticks": 1,
                "points": [tick("warmup", 90), tick("measured", ms), tick("measured", ms)]}})
        };
        let mut fault_tick = tick("measured", 70);
        fault_tick["process_memory"] = json!({"peak_resident_bytes": 1_048_576});
        put_json(
            &root.join("stress/run.json"),
            &json!({"profile": "ci", "hardware": "machine.json",
                "scenario": {"poll_interval_ms": 5000, "max_oldest_lag_blocks": 3, "max_http_latency_ms": 250},
                "results": [point(32, 58), point(128, 107)],
                "faults": [{"file": "fault-rpc", "status": "recovered", "tenants": 16, "fixture": {
                    "rpc_calls": 70, "rpc_failures": 6, "rpc_delay_ms": 2, "rpc_fail_every": 3, "rpc_fail_until_height": 2,
                    "measured_ticks": 2, "drain_ticks": 6, "points": [fault_tick]}}]}),
        );
        put_json(
            &root.join("stress/machine.json"),
            &json!({"cpu_model": "Test CPU", "effective_cores": 4, "sqlite_version": "3.53.2", "selected_cpu": 0}),
        );
        put(
            &root.join("stress/index.html"),
            r#"<a href="point-32.json">32</a>"#,
        );
        put(&root.join("stress/point-32.json"), "{}");
        put(&root.join("stress/point-32.log"), "");
        let shots = root.join("screenshots");
        fs::create_dir_all(shots.join("images")).unwrap();
        RgbImage::from_pixel(1280, 800, Rgb([255, 255, 255]))
            .save(shots.join("images/paid.png"))
            .unwrap();
        RgbImage::from_pixel(390, 844, Rgb([255, 255, 255]))
            .save(shots.join("images/paid-phone.png"))
            .unwrap();
        let shot = |group: &str, shape: &str, image: &str, retry: u64| {
            json!({"group": group, "stage": "paid", "test": "shows paid", "status": "passed", "retry": retry,
                "report": "../browser/playwright-report/index.html", "shape": shape, "theme": "light", "image": image})
        };
        put_json(
            &shots.join("manifest.json"),
            &json!([
                shot("checkout", "desktop", "images/paid.png", 0),
                shot("checkout", "mobile-portrait", "images/paid-phone.png", 0),
                shot("checkout", "desktop", "images/paid.png", 1),
                shot("pos", "desktop", "images/paid.png", 0),
            ]),
        );
    }

    /// Property and fuzz reports for both engine builds, as their artifacts
    /// lay them out, each property run with its own `JUnit` report.
    fn exploration_artifacts(root: &Path) {
        for (build, cases) in [(Build::Zmq, 1114), (Build::Default, 900)] {
            put_json(
                &root.join(format!(
                    "properties-{build}/target/engine-exploration/properties/report.json"
                )),
                &json!({"revision": SHA, "settings": {"PROPTEST_CASES": "512", "ENGINE_FEATURES": build.features()},
                    "semantic_cases": cases, "semantic_observations": {"sql-denial-reached": 12515}}),
            );
            put(
                &root.join(format!("properties-{build}/target/nextest/ci/junit.xml")),
                junit_xml(&[(
                    "engine",
                    &format!("work::tests::properties::money_matches_the_model_{build}"),
                    900.0,
                    "",
                )]),
            );
            for (target, edges) in [("portfolio", 28476), ("inputs", 6355)] {
                let run = root.join(format!("fuzz/engine-fuzz-{target}-{build}-1"));
                put_json(
                    &run.join(format!(
                        "target/engine-exploration/fuzz/{target}/{build}/1/abc/report.json"
                    )),
                    &json!({"status": "passed", "wall_seconds": 400.4, "corpus": {"files": 9}, "new_unique_inputs": 3,
                        "exploration": {"final": {"coverage": edges + u64::from(build == Build::Default), "executions": 724}, "coverage_growth": 35},
                        "semantic_cases": 2, "semantic_observations": {}}),
                );
                // A corpus beside the reports, as the artifact carries one.
                put(
                    &run.join(format!("fuzz/corpus/{target}/report.json")),
                    "not a report",
                );
            }
        }
    }

    struct Fixture {
        scratch: Scratch,
    }

    impl Fixture {
        fn new() -> Self {
            let scratch = Scratch::new("quality");
            coverage_artifact(&scratch.join("coverage"));
            exploration_artifacts(&scratch.join("runs"));
            Fixture { scratch }
        }
        fn path(&self, rel: &str) -> String {
            self.scratch.join(rel).to_string_lossy().into_owned()
        }
        fn out(&self) -> PathBuf {
            self.scratch.join("site/quality")
        }
        fn build(&self, args: &[&str]) -> Value {
            let out = self.path("site/quality");
            let mut all = vec!["--out", out.as_str()];
            all.extend_from_slice(args);
            assert!(build(&crate::root(), &all).unwrap().succeeded());
            read_json(&self.out().join("data.json")).unwrap()
        }
        fn shipped(&self) -> Vec<String> {
            files_under(&self.out().join("reports"), |_| false)
                .unwrap()
                .iter()
                .map(|p| {
                    p.strip_prefix(self.out())
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect()
        }
    }

    #[test]
    fn coverage_links_each_file_to_its_annotated_source() {
        let f = Fixture::new();
        let data = f.build(&["--coverage", &f.path("coverage")]);
        let engine = data["coverage"]["crates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "engine")
            .unwrap()
            .clone();
        let file = &engine["files"][0];
        assert_eq!(file["path"], "src/scanner.rs");
        assert_eq!(file["lines"], json!({"covered": 80, "total": 90}));
        let link = file["report"].as_str().unwrap();
        assert_eq!(
            link,
            "reports/rust/coverage/home/runner/crates/engine/src/scanner.rs.html"
        );
        assert!(f.out().join(link).is_file());
        assert_eq!(engine["unmeasured"], json!(["crates/engine/src/lib.rs"]));
        assert_eq!(data["coverage"]["totals"]["browser"]["lines"]["total"], 20);
        assert!(data["coverage"]["totals"].get("woocommerce").is_none());
    }

    #[test]
    fn only_the_linked_report_files_and_what_they_need_ship() {
        let f = Fixture::new();
        f.build(&["--coverage", &f.path("coverage")]);
        assert_eq!(
            f.shipped(),
            [
                "reports/browser/base.css",
                "reports/browser/index.html",
                "reports/browser/playwright-report/index.html",
                "reports/browser/sort-arrow-sprite.png",
                "reports/rust/control.js",
                "reports/rust/coverage/home/runner/crates/engine/src/scanner.rs.html",
                "reports/rust/index.html",
                "reports/rust/style.css",
                "reports/stress/index.html",
                "reports/stress/point-32.json",
            ]
        );
    }

    #[test]
    fn test_tools_stay_out_of_the_shipping_figure_and_the_badge() {
        let f = Fixture::new();
        let data = f.build(&["--coverage", &f.path("coverage")]);
        assert_eq!(
            data["coverage"]["shipping"],
            json!({"lines": {"covered": 80, "total": 90}, "branches": {"covered": 8, "total": 9}})
        );
        let wallet = data["coverage"]["crates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "cli-wallet")
            .unwrap()
            .clone();
        assert_eq!(wallet["tool"], true);
        let badge: Value = read_json(&f.out().join("badge.json")).unwrap();
        assert_eq!(badge["message"], "88.9%");
        assert_eq!(badge["color"], "green");
    }

    #[test]
    fn every_test_keeps_its_result() {
        let f = Fixture::new();
        let data = f.build(&["--coverage", &f.path("coverage")]);
        let statuses: Vec<&str> = data["coverage"]["tests"]["rust"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["status"].as_str().unwrap())
            .collect();
        assert_eq!(statuses, ["passed", "failed", "skipped"]);
        let browser = &data["coverage"]["tests"]["browser"][0];
        assert_eq!(browser["kind"], "fixture");
        assert_eq!(browser["name"], "checkout shows the amount");
        assert!(data["coverage"]["tests"]["rust"][0].get("kind").is_none());
    }

    #[test]
    fn stress_run_reads_its_named_hardware_file_and_fault_settings() {
        let f = Fixture::new();
        let stress = f.build(&["--coverage", &f.path("coverage")])["stress"].clone();
        assert_eq!(stress["budget_ms"], 5000);
        assert_eq!(stress["hardware"]["cpu_model"], "Test CPU");
        assert_eq!(
            stress["points"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["tenants"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [32, 128]
        );
        assert_eq!(
            stress["points"][1]["ticks"][1],
            json!({"phase": "measured", "ms": 107, "lagging": 0, "lag": 0})
        );
        let rpc = &stress["faults"][0];
        assert_eq!(
            (
                rpc["name"].as_str(),
                rpc["rpc_fail_every"].as_u64(),
                rpc["rpc_failures"].as_u64()
            ),
            (Some("fault-rpc"), Some(3), Some(6))
        );
        assert_eq!(rpc["peak_mb"], 1.0);
    }

    #[test]
    fn only_the_named_build_of_the_nightly_runs_is_shown_with_its_own_tests() {
        let f = Fixture::new();
        let data = f.build(&["--properties", &f.path("runs"), "--fuzz", &f.path("runs")]);
        assert_eq!(data["properties"]["cases"], 1114);
        assert_eq!(
            data["properties"]["tests"][0]["name"],
            "work::tests::properties::money_matches_the_model_zmq"
        );
        let targets: Vec<&str> = data["fuzz"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["target"].as_str().unwrap())
            .collect();
        assert_eq!(targets, ["portfolio", "inputs"]);
        assert_eq!(data["fuzz"][0]["edges"], 28476);
        let default = f.build(&[
            "--properties",
            &f.path("runs"),
            "--fuzz",
            &f.path("runs"),
            "--feature",
            "default",
        ]);
        assert_eq!(default["properties"]["cases"], 900);
        assert_eq!(
            default["properties"]["tests"][0]["name"],
            "work::tests::properties::money_matches_the_model_default"
        );
        assert_eq!(default["fuzz"][0]["edges"], 28477);
    }

    #[test]
    fn gallery_keeps_a_thumbnail_and_the_full_image_of_each_first_try() {
        let f = Fixture::new();
        let gallery = f.build(&["--coverage", &f.path("coverage")])["gallery"].clone();
        assert_eq!(gallery["total"], 3);
        let stage = &gallery["stages"][0];
        assert_eq!(stage["group"], "checkout");
        assert_eq!(stage["shapes"], json!(["desktop", "mobile-portrait"]));
        let desktop = &stage["images"]["desktop"]["light"];
        assert_eq!(
            (desktop["w"].as_u64(), desktop["fw"].as_u64()),
            (Some(480), Some(1280))
        );
        assert_eq!(stage["images"]["mobile-portrait"]["light"]["w"], 280);
        assert!(f.out().join(desktop["full"].as_str().unwrap()).is_file());
        assert_eq!(
            stage["report"],
            "reports/browser/playwright-report/index.html"
        );
        // The same stage name in another group has images of its own.
        let pos = &gallery["stages"][1];
        assert_eq!(pos["group"], "pos");
        let pos_full = pos["images"]["desktop"]["light"]["full"].as_str().unwrap();
        assert_ne!(pos_full, desktop["full"].as_str().unwrap());
        assert!(f.out().join(pos_full).is_file());
    }

    #[test]
    fn missing_inputs_leave_their_sections_out_but_still_build_the_page() {
        let f = Fixture::new();
        let data = f.build(&[]);
        for key in [
            "coverage",
            "properties",
            "fuzz",
            "scale",
            "stress",
            "gallery",
        ] {
            assert!(data[key].is_null(), "{key}");
        }
        let badge: Value = read_json(&f.out().join("badge.json")).unwrap();
        assert_eq!(badge["message"], "unknown");
        assert!(!fs::read_to_string(f.out().join("index.html"))
            .unwrap()
            .contains("@REPO_URL@"));
        assert!(f.out().join("assets/theme.css").is_file());
    }

    #[test]
    fn a_rebuild_replaces_its_own_files_and_nothing_else() {
        let f = Fixture::new();
        f.build(&["--coverage", &f.path("coverage")]);
        put(&f.out().join("somebody-elses.txt"), "kept");
        f.build(&[]);
        assert!(!f.out().join("reports").exists());
        assert!(!f.out().join("gallery").exists());
        assert!(f.out().join("somebody-elses.txt").is_file());
    }

    #[test]
    fn serve_answers_files_and_refuses_paths_outside_its_folder() {
        let scratch = Scratch::new("quality-serve");
        put(&scratch.join("site/index.html"), "<p>report</p>");
        put(&scratch.join("site/a b.json"), "{}");
        put(&scratch.join("secret.txt"), "outside");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let site = scratch.join("site");
        let requests = 8;
        let server = thread::spawn(move || {
            for stream in listener.incoming().take(requests) {
                respond(&site, stream.unwrap()).unwrap();
            }
        });
        let send = |line: &str| {
            let mut stream = TcpStream::connect(addr).unwrap();
            write!(stream, "{line} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
            let mut reply = String::new();
            stream.read_to_string(&mut reply).unwrap();
            reply
        };
        let page = send("GET /");
        assert!(
            page.starts_with("HTTP/1.1 200")
                && page.contains("text/html")
                && page.ends_with("<p>report</p>"),
            "{page}"
        );
        assert!(send("GET /a%20b.json").starts_with("HTTP/1.1 200"));
        assert!(send("GET /missing.json").starts_with("HTTP/1.1 404"));
        for outside in ["/../secret.txt", "/%2e%2e/secret.txt", "/..%2fsecret.txt"] {
            assert!(
                send(&format!("GET {outside}")).starts_with("HTTP/1.1 404"),
                "{outside}"
            );
        }
        let head = send("HEAD /");
        assert!(
            head.starts_with("HTTP/1.1 200") && head.ends_with("\r\n\r\n"),
            "{head}"
        );
        let post = send("POST /");
        assert!(
            post.starts_with("HTTP/1.1 405") && post.contains("Allow: GET, HEAD"),
            "{post}"
        );
        server.join().unwrap();
    }

    #[test]
    fn the_sources_pick_only_the_artifacts_the_report_reads() {
        let pick = |key: &str, name: &str, build: Build| {
            (SOURCES.iter().find(|s| s.key == key).unwrap().wanted)(name, build)
        };
        assert!(pick("coverage", &format!("coverage-{SHA}"), Build::Zmq));
        assert!(!pick(
            "coverage",
            &format!("coverage-rust-{SHA}"),
            Build::Zmq
        ));
        assert!(pick(
            "fuzz",
            "engine-fuzz-portfolio-zmq-37702639422",
            Build::Zmq
        ));
        assert!(!pick(
            "fuzz",
            "engine-fuzz-portfolio-default-37702639422",
            Build::Zmq
        ));
        assert!(pick(
            "fuzz",
            "engine-fuzz-portfolio-default-37702639422",
            Build::Default
        ));
        assert!(pick(
            "properties",
            "engine-properties-zmq-37695839811",
            Build::Zmq
        ));
        assert!(!pick(
            "properties",
            "engine-properties-default-37695839811",
            Build::Zmq
        ));
        assert!(pick(
            "properties",
            "engine-properties-default-37695839811",
            Build::Default
        ));
    }
}
