//! The CI artifacts the report is built from, read into what the pages show:
//! test results, coverage, the stress and scale runs, the nightly property
//! and fuzz runs, and the screenshots.

use crate::coverage::{Counts, CrateCoverage, Status as CollectorStatus};
use crate::exploration::{Build, Status as CampaignStatus};
use crate::live;
use crate::summary::skip_reason;
use crate::support::{at, css_links, files_under, html_links, read_json};
use image::{codecs::jpeg::JpegEncoder, imageops::FilterType, DynamicImage};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    io::{self, Write},
    path::{Component as PathPart, Path, PathBuf},
    thread,
};

/// Crates that only exist to test the others: shown, but kept out of the
/// shipping figure and the badge.
const TEST_TOOLS: [&str; 4] = [
    "cli-wallet",
    "e2e-harness",
    "engine-test-support",
    "mock-woocommerce",
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
/// Folders of the exploration artifacts that hold no reports: the corpus
/// and crash inputs run to thousands of files.
const NOT_REPORTS: [&str; 4] = ["corpus", "artifacts", "semantics", "calibration-semantics"];

/// The artifact files the pages link, and so the roots of what ships.
#[derive(Default)]
pub(super) struct Linked(BTreeSet<String>);

impl Linked {
    /// Adds an artifact file the pages link, and returns the link to its
    /// copy from the site; nothing when the path climbs out of the artifact,
    /// so no page links outside `reports/`.
    fn link(&mut self, rel: &str) -> Option<String> {
        let clean = normalise(Path::new(rel))?;
        let href = format!("reports/{clean}");
        self.0.insert(clean);
        Some(href)
    }

    pub(super) fn extend(&mut self, other: Linked) {
        self.0.extend(other.0);
    }
}

/// One of each coverage component, where that component left something.
#[derive(Default)]
pub(super) struct PerComponent<T> {
    pub(super) rust: Option<T>,
    pub(super) browser: Option<T>,
    pub(super) woocommerce: Option<T>,
}

/// Line and branch counts together, as the pages show them.
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub(super) struct Coverage {
    pub(super) lines: Counts,
    pub(super) branches: Counts,
}

impl Coverage {
    fn add(&mut self, other: Coverage) {
        self.lines.covered += other.lines.covered;
        self.lines.total += other.lines.total;
        self.branches.covered += other.branches.covered;
        self.branches.total += other.branches.total;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TestStatus {
    Passed,
    Failed,
    Skipped,
}

/// Which browser project a test ran in: against the fixture server or the
/// real binaries, in whichever browser (`real-binaries-webkit`).
/// Playwright's `JUnit` report names it as each testsuite's `hostname`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum BrowserKind {
    Fixture,
    RealBinaries,
}

impl BrowserKind {
    fn of_project(project: &str) -> Self {
        if project.starts_with("real-binaries") {
            BrowserKind::RealBinaries
        } else {
            BrowserKind::Fixture
        }
    }
}

/// One test case of a `JUnit` report.
pub(crate) struct Test {
    pub(crate) class: String,
    pub(crate) name: String,
    pub(super) secs: f64,
    pub(crate) status: TestStatus,
    pub(super) kind: Option<BrowserKind>,
    /// The `hostname` of its testsuite: the Playwright project, or empty.
    pub(super) project: String,
    /// Why it failed or was skipped, where the report says.
    pub(super) message: Option<String>,
}

pub(crate) fn junit(path: &Path) -> io::Result<Vec<Test>> {
    let text = fs::read_to_string(path).map_err(|e| at(path, e))?;
    let doc = roxmltree::Document::parse(&text)
        .map_err(|e| at(path, io::Error::new(io::ErrorKind::InvalidData, e)))?;
    Ok(doc
        .descendants()
        .filter(|n| n.has_tag_name("testcase"))
        .map(|case| {
            let child = |tag: &str| case.children().find(|c| c.has_tag_name(tag));
            let (status, message) =
                if let Some(failure) = child("failure").or_else(|| child("error")) {
                    let message = [failure.attribute("message"), failure.text()]
                        .into_iter()
                        .flatten()
                        .map(str::trim)
                        .find(|m| !m.is_empty())
                        .map(str::to_string);
                    (TestStatus::Failed, message)
                } else if let Some(skipped) = child("skipped") {
                    (TestStatus::Skipped, skip_reason(case, skipped))
                } else {
                    (TestStatus::Passed, None)
                };
            let secs = case
                .attribute("time")
                .and_then(|t| t.parse::<f64>().ok())
                .filter(|s| s.is_finite() && *s >= 0.0)
                .unwrap_or(0.0);
            Test {
                class: case.attribute("classname").unwrap_or("").to_string(),
                name: case.attribute("name").unwrap_or("").to_string(),
                secs,
                status,
                kind: None,
                project: case
                    .ancestors()
                    .find(|n| n.has_tag_name("testsuite"))
                    .and_then(|suite| suite.attribute("hostname"))
                    .unwrap_or("")
                    .to_string(),
                message,
            }
        })
        .collect())
}

/// One source file of a crate, its annotated page relative to the site.
pub(super) struct FileRow {
    pub(super) path: String,
    pub(super) coverage: Coverage,
    /// Nothing when the artifact named a page outside itself.
    pub(super) report: Option<String>,
}

pub(super) struct Crate {
    pub(super) name: String,
    pub(super) coverage: Coverage,
    /// A test tool, kept out of the shipping figure.
    pub(super) tool: bool,
    pub(super) files: Vec<FileRow>,
    /// Source files no test profile instrumented, relative to the repository.
    pub(super) unmeasured: Vec<String>,
}

#[derive(Default)]
pub(super) struct Tests {
    pub(super) rust: Vec<Test>,
    pub(super) browser: Vec<Test>,
    pub(super) woocommerce: Vec<Test>,
}

/// The report pages shipped under reports/, relative to the site.
#[derive(Default)]
pub(super) struct Reports {
    pub(super) rust: Option<String>,
    pub(super) browser: Option<String>,
    pub(super) woocommerce: Option<String>,
    pub(super) stress: Option<String>,
}

/// The coverage artifact as the pages show it.
pub(super) struct CoverageRun {
    /// Whether every collector passed.
    pub(super) passed: bool,
    pub(super) revision: Option<String>,
    /// The figures of each component that left a manifest.
    pub(super) totals: PerComponent<Coverage>,
    /// The tool each component measured with.
    pub(super) tools: PerComponent<String>,
    pub(super) crates: Vec<Crate>,
    pub(super) tests: Tests,
    pub(super) reports: Reports,
    /// The Rust crates that ship, added up: the headline figure and the badge.
    pub(super) shipping: Coverage,
}

/// A manifest's figures, as every coverage component records them.
#[derive(Deserialize)]
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

/// run.json: how each collector ended.
#[derive(Deserialize)]
struct Run {
    revision: Option<String>,
    #[serde(default)]
    components: Vec<RunComponent>,
}

#[derive(Deserialize)]
struct RunComponent {
    status: CollectorStatus,
}

/// Every suite's results from the `JUnit` reports the collectors left.
fn test_results(src: &Path) -> io::Result<Tests> {
    let read = |rel: &str| {
        let path = src.join(rel);
        if path.is_file() {
            junit(&path)
        } else {
            Ok(Vec::new())
        }
    };
    // One run of both browser projects.
    let browser = read("browser/junit.xml")?
        .into_iter()
        .map(|t| Test {
            kind: Some(BrowserKind::of_project(&t.project)),
            ..t
        })
        .collect();
    Ok(Tests {
        rust: read("rust/junit.xml")?,
        browser,
        woocommerce: read("woocommerce/junit.xml")?,
    })
}

fn manifest(src: &Path, name: &str) -> io::Result<Option<Manifest>> {
    let path = src.join(format!("{name}.json"));
    if path.is_file() {
        read_json(&path).map(Some)
    } else {
        Ok(None)
    }
}

/// Coverage totals, per-crate and per-file Rust figures, and every test
/// result; the report pages they link.
pub(super) fn coverage(src: &Path) -> io::Result<(CoverageRun, Linked)> {
    let run: Run = read_json(&src.join("run.json"))?;
    let mut totals = PerComponent::default();
    let mut tools = PerComponent::default();
    for (name, total, tool) in [
        ("rust", &mut totals.rust, &mut tools.rust),
        ("browser", &mut totals.browser, &mut tools.browser),
        (
            "woocommerce",
            &mut totals.woocommerce,
            &mut tools.woocommerce,
        ),
    ] {
        if let Some(m) = manifest(src, name)? {
            *total = Some(Coverage {
                lines: m.lines,
                branches: m.branches,
            });
            *tool = m.tools.collector;
        }
    }
    let mut linked = Linked::default();
    let mut crates = Vec::new();
    let summaries = src.join("rust-crates.json");
    if summaries.is_file() {
        let summaries: Vec<CrateCoverage> = read_json(&summaries)?;
        for c in summaries {
            let files = c
                .files
                .into_iter()
                .map(|f| FileRow {
                    report: linked.link(&f.report),
                    path: f.path,
                    coverage: Coverage {
                        lines: f.lines,
                        branches: f.branches,
                    },
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
    let mut reports = Reports::default();
    for (name, report) in [
        ("rust", &mut reports.rust),
        ("browser", &mut reports.browser),
        ("woocommerce", &mut reports.woocommerce),
        ("stress", &mut reports.stress),
    ] {
        let index = format!("{name}/index.html");
        if src.join(&index).is_file() {
            *report = linked.link(&index);
        }
    }
    let mut shipping = Coverage::default();
    for c in crates.iter().filter(|c| !c.tool) {
        shipping.add(c.coverage);
    }
    Ok((
        CoverageRun {
            passed: run
                .components
                .iter()
                .all(|c| c.status == CollectorStatus::Passed),
            revision: run.revision,
            totals,
            tools,
            crates,
            tests: test_results(src)?,
            reports,
            shipping,
        },
        linked,
    ))
}

/// `a/b/../c` as `a/c`; nothing when the path climbs out of the root.
fn normalise(path: &Path) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for part in path.components() {
        match part {
            PathPart::ParentDir => {
                parts.pop()?;
            }
            PathPart::Normal(p) => parts.push(p.to_string_lossy().into_owned()),
            PathPart::CurDir => {}
            PathPart::RootDir | PathPart::Prefix(_) => return None,
        }
    }
    Some(parts.join("/"))
}

/// Copies into `out/reports/` the artifact files the pages link, and every
/// file those pages and stylesheets link in turn, so the reports work and
/// nothing unlinked ships.
pub(super) fn ship_reports(src: &Path, out: &Path, linked: &Linked) -> io::Result<()> {
    // Links that climb out of the artifact are dropped, here and below, so
    // nothing outside it is read or written outside `out/reports/`.
    let mut queue: VecDeque<String> = linked
        .0
        .iter()
        .filter_map(|rel| normalise(Path::new(rel)))
        .collect();
    let mut shipped = BTreeSet::new();
    while let Some(rel) = queue.pop_front() {
        if !shipped.insert(rel.clone()) {
            continue;
        }
        let from = src.join(&rel);
        if !from.is_file() {
            // A report may link what its generator never wrote; the pages'
            // own links were checked by the artifact's validation.
            continue;
        }
        let to = out.join("reports").join(&rel);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(|e| at(parent, e))?;
        }
        fs::copy(&from, &to).map_err(|e| at(&from, e))?;
        let links = match from.extension().and_then(|e| e.to_str()) {
            Some("html") => html_links(&fs::read_to_string(&from).map_err(|e| at(&from, e))?),
            Some("css") => css_links(&fs::read_to_string(&from).map_err(|e| at(&from, e))?),
            _ => continue,
        };
        let dir = Path::new(&rel).parent().unwrap_or(Path::new(""));
        queue.extend(links.iter().filter_map(|link| normalise(&dir.join(link))));
    }
    Ok(())
}

/// Where a stress run is in its scan rounds.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub(super) enum Phase {
    Warmup,
    Measured,
    Drain,
}

#[derive(Deserialize)]
struct Memory {
    peak_resident_bytes: Option<u64>,
}

/// One scan round of a stress point: how long, and how many stores it
/// left behind.
#[derive(Deserialize)]
pub(super) struct Tick {
    pub(super) phase: Phase,
    #[serde(rename = "duration_ms")]
    pub(super) ms: u64,
    #[serde(rename = "lagging_tenants")]
    pub(super) lagging: u64,
    process_memory: Option<Memory>,
}

/// What the stress fixture measured and was set to at one point.
#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct Fixture {
    tenants: Option<u64>,
    pub(super) points: Vec<Tick>,
    #[serde(rename = "http_max_latency_us")]
    pub(super) http_max_us: Option<u64>,
    pub(super) rpc_calls: Option<u64>,
    pub(super) rpc_failures: Option<u64>,
    pub(super) rpc_delay_ms: Option<u64>,
    pub(super) rpc_fail_every: Option<u64>,
    pub(super) rpc_fail_until_height: Option<u64>,
    pub(super) custody_slots: Option<u64>,
    pub(super) custody_delay_ms: Option<u64>,
    #[serde(rename = "custody_scans_completed")]
    pub(super) custody_scans: Option<u64>,
    pub(super) custody_max_wait_us: Option<u64>,
    #[serde(rename = "write_lock_hold_ms_per_tick")]
    pub(super) lock_ms: Option<u64>,
    pub(super) measured_ticks: Option<u64>,
    pub(super) drain_ticks: Option<u64>,
}

/// The faults a stress run injects, by the file it names each run after.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Fault {
    Rpc,
    SqliteLock,
    Custody,
}

/// One measured point of a stress run, or one fault run.
#[derive(Deserialize)]
pub(super) struct StressPoint {
    file: Option<String>,
    pub(super) status: Option<String>,
    tenants: Option<u64>,
    peak_resident_bytes: Option<u64>,
    #[serde(default)]
    pub(super) fixture: Fixture,
}

impl StressPoint {
    pub(super) fn tenants(&self) -> Option<u64> {
        self.tenants.or(self.fixture.tenants)
    }

    pub(super) fn ticks(&self) -> &[Tick] {
        &self.fixture.points
    }

    /// The most memory the process held, from the run or its rounds.
    pub(super) fn peak_bytes(&self) -> Option<u64> {
        self.peak_resident_bytes.or_else(|| {
            self.ticks()
                .iter()
                .filter_map(|t| t.process_memory.as_ref()?.peak_resident_bytes)
                .max()
        })
    }

    pub(super) fn fault(&self) -> Option<Fault> {
        match self.file.as_deref()? {
            "fault-rpc" => Some(Fault::Rpc),
            "fault-sqlite-lock" => Some(Fault::SqliteLock),
            "fault-custody" => Some(Fault::Custody),
            _ => None,
        }
    }

    /// Whether the store count kept up with the blocks.
    pub(super) fn sustainable(&self) -> bool {
        self.status.as_deref() == Some("sustainable")
    }
}

/// The limits a stress run measures against.
#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct Scenario {
    #[serde(rename = "poll_interval_ms")]
    pub(super) budget_ms: Option<u64>,
    #[serde(rename = "max_oldest_lag_blocks")]
    pub(super) max_lag_blocks: Option<u64>,
    #[serde(rename = "max_http_latency_ms")]
    pub(super) max_http_ms: Option<u64>,
    #[serde(rename = "fault_sqlite_lock_ms")]
    pub(super) fault_lock_ms: Option<u64>,
}

/// The machine a stress run measured on.
#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct Hardware {
    pub(super) cpu_model: Option<String>,
    pub(super) effective_cores: Option<u64>,
    pub(super) sqlite_version: Option<String>,
}

/// run.json names the machine inline, or the file beside it that holds it.
#[derive(Deserialize)]
#[serde(untagged)]
enum HardwareSource {
    Inline(Hardware),
    File(String),
}

/// The one-CPU stress run (CI) or the weekly scale run.
#[derive(Deserialize)]
pub(super) struct Stress {
    #[serde(default)]
    pub(super) scenario: Scenario,
    #[serde(default, rename = "results")]
    pub(super) points: Vec<StressPoint>,
    #[serde(default)]
    pub(super) faults: Vec<StressPoint>,
    #[serde(default, rename = "hardware")]
    hardware_source: Option<HardwareSource>,
    #[serde(skip)]
    pub(super) hardware: Hardware,
}

pub(super) fn stress(run_file: &Path) -> io::Result<Stress> {
    let mut run: Stress = read_json(run_file)?;
    run.hardware = match run.hardware_source.take() {
        Some(HardwareSource::Inline(hardware)) => hardware,
        Some(HardwareSource::File(file)) => {
            let beside = run_file.with_file_name(file);
            if beside.is_file() {
                read_json(&beside)?
            } else {
                Hardware::default()
            }
        }
        None => Hardware::default(),
    };
    Ok(run)
}

/// The report files of an exploration artifact, leaving its corpora alone.
pub(super) fn report_files(src: &Path) -> io::Result<Vec<PathBuf>> {
    files_under(src, |dir| {
        dir.file_name()
            .is_some_and(|n| NOT_REPORTS.iter().any(|skip| n == *skip))
    })
}

/// The settings a property run was made with, as its report records them.
#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct Settings {
    #[serde(rename = "PROPTEST_CASES")]
    pub(super) proptest_cases: Option<String>,
    #[serde(rename = "ENGINE_FEATURES")]
    engine_features: Option<String>,
}

/// A count of each scenario the engine records while running generated
/// histories, by its name.
pub(super) type Observations = BTreeMap<String, u64>;

#[derive(Deserialize)]
struct PropertyReport {
    #[serde(default)]
    settings: Settings,
    #[serde(default)]
    semantic_cases: u64,
    #[serde(default)]
    semantic_observations: Observations,
}

/// The nightly property run of one build.
pub(super) struct Properties {
    pub(super) settings: Settings,
    pub(super) cases: u64,
    pub(super) observations: Observations,
    pub(super) tests: Vec<Test>,
}

/// The nightly property run of one build: its settings, scenario counts and
/// every property, from the `JUnit` report beside its `report.json`.
pub(super) fn properties(src: &Path, build: Build) -> io::Result<Option<Properties>> {
    for path in report_files(src)?
        .iter()
        .filter(|p| p.ends_with("engine-exploration/properties/report.json"))
    {
        let report: PropertyReport = read_json(path)?;
        if report.settings.engine_features.as_deref() != Some(build.features()) {
            continue;
        }
        // .../target/engine-exploration/properties/report.json and
        // .../target/nextest/ci/junit.xml come from the same run.
        let junit_file = path
            .ancestors()
            .nth(3)
            .map(|target| target.join("nextest/ci/junit.xml"))
            .filter(|p| p.is_file());
        return Ok(Some(Properties {
            settings: report.settings,
            cases: report.semantic_cases,
            observations: report.semantic_observations,
            tests: match junit_file {
                Some(path) => junit(&path)?,
                None => Vec::new(),
            },
        }));
    }
    Ok(None)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Corpus {
    files: Option<u64>,
}

#[derive(Deserialize)]
struct Sample {
    coverage: u64,
    executions: u64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Exploration {
    #[serde(rename = "final")]
    last: Option<Sample>,
    coverage_growth: Option<i64>,
}

#[derive(Deserialize)]
struct FuzzReport {
    status: CampaignStatus,
    #[serde(default)]
    wall_seconds: f64,
    #[serde(default)]
    corpus: Corpus,
    new_unique_inputs: Option<u64>,
    #[serde(default)]
    exploration: Exploration,
    #[serde(default)]
    semantic_cases: u64,
    #[serde(default)]
    semantic_observations: Observations,
}

/// Last night's campaign of one fuzz target.
pub(super) struct FuzzTarget {
    pub(super) target: String,
    pub(super) status: CampaignStatus,
    pub(super) seconds: f64,
    pub(super) corpus: Option<u64>,
    pub(super) new_inputs: Option<u64>,
    /// Code edges found, and how many were new.
    pub(super) edges: Option<u64>,
    pub(super) edge_growth: Option<i64>,
    pub(super) executions: Option<u64>,
    pub(super) cases: u64,
    pub(super) observations: Observations,
}

/// Last night's campaign for each fuzz target of one build, deepest first.
pub(super) fn fuzz(src: &Path, build: Build) -> io::Result<Vec<FuzzTarget>> {
    let mut targets: BTreeMap<String, FuzzTarget> = BTreeMap::new();
    for path in report_files(src)?
        .iter()
        .filter(|p| p.ends_with("report.json"))
    {
        // .../engine-exploration/fuzz/<target>/<build>/<seed>/<id>/report.json
        let parts: Vec<&str> = path
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .collect();
        let [.., "engine-exploration", "fuzz", target, run_build, _seed, _id, "report.json"] =
            parts.as_slice()
        else {
            continue;
        };
        if *run_build != build.name() {
            continue;
        }
        let r: FuzzReport = read_json(path)?;
        let last = r.exploration.last;
        targets.insert(
            (*target).to_string(),
            FuzzTarget {
                target: (*target).to_string(),
                status: r.status,
                seconds: r.wall_seconds,
                corpus: r.corpus.files,
                new_inputs: r.new_unique_inputs,
                edges: last.as_ref().map(|s| s.coverage),
                edge_growth: r.exploration.coverage_growth,
                executions: last.as_ref().map(|s| s.executions),
                cases: r.semantic_cases,
                observations: r.semantic_observations,
            },
        );
    }
    let mut list: Vec<FuzzTarget> = targets.into_values().collect();
    list.sort_by_key(|t| std::cmp::Reverse(t.edges.unwrap_or(0)));
    Ok(list)
}

/// The sizes the browser tests photograph a screen at, in the order the
/// gallery offers them.
#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Shape {
    Desktop,
    Element,
    AsIs,
    TabletLandscape,
    TabletPortrait,
    MobileLandscape,
    MobilePortrait,
    SmallPortrait,
}

impl Shape {
    pub(super) fn name(self) -> &'static str {
        match self {
            Shape::Desktop => "desktop",
            Shape::Element => "element",
            Shape::AsIs => "as-is",
            Shape::TabletLandscape => "tablet-landscape",
            Shape::TabletPortrait => "tablet-portrait",
            Shape::MobileLandscape => "mobile-landscape",
            Shape::MobilePortrait => "mobile-portrait",
            Shape::SmallPortrait => "small-portrait",
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Shape::Desktop => "Desktop",
            Shape::Element => "Element",
            Shape::AsIs => "As rendered",
            Shape::TabletLandscape => "Tablet landscape",
            Shape::TabletPortrait => "Tablet portrait",
            Shape::MobileLandscape => "Phone landscape",
            Shape::MobilePortrait => "Phone portrait",
            Shape::SmallPortrait => "Small phone",
        }
    }

    fn thumb_width(self) -> u32 {
        match self {
            Shape::MobilePortrait | Shape::SmallPortrait => PHONE_THUMB_WIDTH,
            Shape::Element => ELEMENT_THUMB_WIDTH,
            _ => THUMB_WIDTH,
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[serde(rename_all = "lowercase")]
pub(super) enum Theme {
    Light,
    Dark,
}

impl Theme {
    pub(super) fn name(self) -> &'static str {
        match self {
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }
}

/// A screenshot as the grid's thumbnail and the viewer's full image, both
/// relative to the site.
#[derive(Serialize, Clone)]
pub(super) struct Image {
    pub(super) thumb: String,
    pub(super) w: u32,
    pub(super) h: u32,
    pub(super) full: String,
    pub(super) fw: u32,
    pub(super) fh: u32,
}

fn save_jpeg(image: &DynamicImage, path: &Path, quality: u8) -> io::Result<()> {
    let mut file = io::BufWriter::new(fs::File::create(path).map_err(|e| at(path, e))?);
    image
        .to_rgb8()
        .write_with_encoder(JpegEncoder::new_with_quality(&mut file, quality))
        .map_err(|e| at(path, io::Error::other(e)))?;
    file.flush().map_err(|e| at(path, e))
}

/// One screenshot as the viewer's full image and the grid's thumbnail.
fn convert(image: &Path, shape: Shape, name: &str, out: &Path) -> io::Result<Image> {
    let full = image::open(image).map_err(|e| at(image, io::Error::other(e)))?;
    save_jpeg(&full, &out.join("gallery/full").join(name), FULL_QUALITY)?;
    let width = shape.thumb_width().min(full.width()).max(1);
    let height = u64::from(full.height()) * u64::from(width) / u64::from(full.width().max(1));
    let height = u32::try_from(height.max(1)).unwrap_or(u32::MAX);
    let thumb = full.resize_exact(width, height, FilterType::CatmullRom);
    save_jpeg(&thumb, &out.join("gallery/thumb").join(name), THUMB_QUALITY)?;
    Ok(Image {
        thumb: format!("gallery/thumb/{name}"),
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
    shape: Shape,
    theme: Theme,
    image: String,
    #[serde(default)]
    retry: u64,
}

/// A screen the browser tests photographed: one stage of one test group,
/// in every shape and theme it was taken in.
pub(super) struct Screen {
    pub(super) group: String,
    pub(super) stage: String,
    pub(super) test: Option<String>,
    /// The worst result among its shots; `passed` when all passed.
    pub(super) status: Option<String>,
    /// The Playwright report the test is in, relative to the site.
    pub(super) report: Option<String>,
    /// By shape, in the gallery's order, then theme.
    pub(super) images: BTreeMap<Shape, BTreeMap<Theme, Image>>,
    /// Screenshots taken, retries left out.
    pub(super) count: usize,
}

impl Screen {
    pub(super) fn passed(&self) -> bool {
        self.status.as_deref() == Some("passed")
    }

    pub(super) fn shapes(&self) -> impl Iterator<Item = Shape> + '_ {
        self.images.keys().copied()
    }

    /// The image of a shape in a theme, falling back to whichever theme it
    /// was taken in.
    pub(super) fn image(&self, shape: Shape, theme: Theme) -> Option<&Image> {
        let themes = self.images.get(&shape)?;
        themes.get(&theme).or_else(|| themes.values().next())
    }
}

pub(super) struct Gallery {
    /// In group, then stage order.
    pub(super) screens: Vec<Screen>,
    /// Screenshots taken, retries left out.
    pub(super) total: usize,
}

/// Text as a file name part: ASCII letters, digits and dashes only, so no
/// manifest value can name a path.
fn slug(text: &str) -> String {
    let slug: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    slug.trim_matches('-').to_string()
}

/// A screenshot waiting to be converted, and where its images go.
struct Conversion {
    screen: usize,
    shape: Shape,
    theme: Theme,
    image: PathBuf,
    name: String,
}

/// Every screenshot as a thumbnail for the grid and the full image for the
/// viewer; the Playwright reports the screens link.
pub(super) fn gallery(src: &Path, out: &Path) -> io::Result<Option<(Gallery, Linked)>> {
    let manifest = src.join("screenshots/manifest.json");
    if !manifest.is_file() {
        return Ok(None);
    }
    let shots: Vec<Shot> = read_json(&manifest)?;
    let shots: Vec<&Shot> = shots.iter().filter(|s| s.retry == 0).collect();
    for dir in ["gallery/full", "gallery/thumb"] {
        let dir = out.join(dir);
        fs::create_dir_all(&dir).map_err(|e| at(&dir, e))?;
    }
    let mut screens: Vec<Screen> = Vec::new();
    let mut index: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    let mut linked = Linked::default();
    // The first shot of each screen's shape and theme is the one shown.
    let mut seen: BTreeSet<(usize, Shape, Theme)> = BTreeSet::new();
    let mut jobs: Vec<Conversion> = Vec::new();
    for shot in &shots {
        let i = *index.entry((&shot.group, &shot.stage)).or_insert_with(|| {
            // The manifest names the report from the screenshots folder.
            let report = shot
                .report
                .as_deref()
                .and_then(|r| linked.link(&format!("screenshots/{r}")));
            screens.push(Screen {
                group: shot.group.clone(),
                stage: shot.stage.clone(),
                test: shot.test.clone(),
                status: shot.status.clone(),
                report,
                images: BTreeMap::new(),
                count: 0,
            });
            screens.len() - 1
        });
        let screen = &mut screens[i];
        screen.count += 1;
        if shot.status.as_deref().is_some_and(|s| s != "passed") {
            screen.status.clone_from(&shot.status);
        }
        if !seen.insert((i, shot.shape, shot.theme)) {
            continue;
        }
        jobs.push(Conversion {
            screen: i,
            image: src
                .join("screenshots")
                .join(normalise(Path::new(&shot.image)).ok_or_else(|| {
                    at(
                        &manifest,
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "screenshot {} is outside the screenshots folder",
                                shot.image
                            ),
                        ),
                    )
                })?),
            // The screen's number keeps names apart (stages repeat across
            // groups); the rest is only for reading, kept to a safe slug.
            name: format!(
                "{i}-{}-{}-{}.jpg",
                slug(&shot.stage),
                shot.shape.name(),
                shot.theme.name()
            ),
            shape: shot.shape,
            theme: shot.theme,
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
                        .map(|job| convert(&job.image, job.shape, &job.name, out))
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
            .entry(job.shape)
            .or_default()
            .insert(job.theme, image?);
    }
    screens.sort_by(|a, b| (&a.group, &a.stage).cmp(&(&b.group, &b.stage)));
    Ok(Some((
        Gallery {
            screens,
            total: shots.len(),
        },
        linked,
    )))
}

/// One test of the live-network run, with what it needs from outside.
pub(super) struct LiveTest {
    pub(super) test: Test,
    /// Rust or Browser.
    pub(super) suite: &'static str,
    /// The services it needs, by name.
    pub(super) needs: Vec<String>,
    /// For a failure, the service it needs that wasn't answering when the
    /// run checked: the failure is that service's, not the code's.
    pub(super) unreachable: Option<String>,
}

/// The live-network artifact as the page shows it.
pub(super) struct LiveRun {
    pub(super) tests: Vec<LiveTest>,
    pub(super) services: Vec<live::Check>,
    pub(super) wallets: Option<live::Wallets>,
}

impl LiveRun {
    /// The failures that are the code's: every service each needs answered.
    pub(super) fn failures(&self) -> impl Iterator<Item = &LiveTest> {
        self.tests
            .iter()
            .filter(|t| t.test.status == TestStatus::Failed && t.unreachable.is_none())
    }
}

/// The live-network run (`cargo xtask live`): its two `JUnit` reports, and
/// `live.json` with the services, the wallets and each test's reason. A
/// run that left neither report shows nothing.
pub(super) fn live(src: &Path) -> io::Result<Option<LiveRun>> {
    let read = |name: &str| {
        let path = src.join(name);
        if path.is_file() {
            junit(&path).map(Some)
        } else {
            Ok(None)
        }
    };
    let (rust, browser) = (read("rust-junit.xml")?, read("browser-junit.xml")?);
    if rust.is_none() && browser.is_none() {
        return Ok(None);
    }
    let summary = src.join("live.json");
    let summary: live::Live = if summary.is_file() {
        read_json(&summary)?
    } else {
        live::Live::default()
    };
    let name_of = |id: &str| {
        summary
            .services
            .iter()
            .find(|c| c.id == id)
            .map_or_else(|| id.to_string(), |c| c.name.clone())
    };
    let tests = [("Rust", rust), ("Browser", browser)]
        .into_iter()
        .flat_map(|(suite, tests)| tests.into_iter().flatten().map(move |t| (suite, t)))
        .map(|(suite, test)| {
            let reason = summary
                .reasons
                .get(&live::case_key(&test.class, &test.name))
                .map_or("", String::as_str);
            let unreachable = (test.status == TestStatus::Failed)
                .then(|| live::missing(reason, &summary.services))
                .flatten()
                .map(|c| c.name.clone());
            LiveTest {
                needs: live::needs(reason).iter().map(|id| name_of(id)).collect(),
                unreachable,
                suite,
                test,
            }
        })
        .collect();
    Ok(Some(LiveRun {
        tests,
        services: summary.services,
        wallets: summary.wallets,
    }))
}
