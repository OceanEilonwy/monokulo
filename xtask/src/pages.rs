//! `cargo xtask pages build`: the quality report (GitHub Pages: /quality/)
//! from CI artifacts, and `cargo xtask pages fetch`: those artifacts,
//! downloaded from main's newest runs.
//!
//! Every input of the report is optional; the page says what it has no data
//! for. It writes the page (web/pages/quality/index.html), its data (data.json),
//! the screenshot gallery (gallery/), the annotated coverage reports
//! (reports/) and a shields.io endpoint for the coverage badge (badge.json).
//! Only one engine build of the property and fuzz runs is shown (ZMQ unless
//! told otherwise): they run each build separately.

use image::{codecs::jpeg::JpegEncoder, imageops::FilterType, DynamicImage};
use serde_json::{json, Map, Value};
use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, Write},
    path::{Component, Path, PathBuf},
    process::Command,
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

pub(crate) const HELP: &str = "\
        pages build --out DIR [--coverage DIR] [--properties DIR] [--fuzz DIR] [--scale DIR]\n\
                     [--sources FILE] [--feature zmq|default] [--repo-url URL]\n\
                      Build the quality report (GitHub Pages /quality/) from CI artifacts: the joined\n\
                      coverage artifact (or target/coverage), an engine-properties artifact, a folder of\n\
                      engine-fuzz artifacts, an engine-scale-measurements artifact, and a JSON file naming\n\
                      the run behind each (docs/COVERAGE.md)\n\
        pages fetch DIR [--repo OWNER/NAME]\n\
                      Download main's newest OpenWrt site, coverage, property, fuzz and scale artifacts\n\
                      into DIR, with DIR/sources.json naming their runs (needs the gh CLI)\n\
        serve DIR [PORT]\n\
                      Serve DIR on http://127.0.0.1:PORT (8000) to look at a built report: the page\n\
                      reads data.json, which a file opened from disk can't";

/// Serves a built report from `dir` until stopped: GET only, files only,
/// nothing outside `dir`.
pub(crate) fn serve(args: &[&str]) -> io::Result<bool> {
    let (dir, port) = match args {
        [dir] => (PathBuf::from(dir), "8000"),
        [dir, port] => (PathBuf::from(dir), *port),
        _ => return Err(io::Error::other("usage: cargo xtask serve DIR [PORT]")),
    };
    let listener = std::net::TcpListener::bind(format!("127.0.0.1:{port}"))?;
    eprintln!(
        "serving {} on http://127.0.0.1:{port}/ (Ctrl+C to stop)",
        dir.display()
    );
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if let Err(e) = respond(&dir, stream) {
            eprintln!("serve: {e}");
        }
    }
    Ok(true)
}

fn respond(dir: &Path, mut stream: std::net::TcpStream) -> io::Result<()> {
    use std::io::{BufRead, BufReader};
    let mut request = String::new();
    BufReader::new(&stream).read_line(&mut request)?;
    let target = request.split_whitespace().nth(1).unwrap_or("/");
    let path = target.split(['?', '#']).next().unwrap_or("/");
    let path = path.replace("%20", " ");
    let mut file = dir.to_path_buf();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        if part == ".." {
            file = PathBuf::new();
            break;
        }
        file.push(part);
    }
    if file.is_dir() {
        file.push("index.html");
    }
    let (status, body, kind) = match fs::read(&file) {
        Ok(body) if file.starts_with(dir) => {
            let kind = match file.extension().and_then(|e| e.to_str()).unwrap_or("") {
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
            };
            ("200 OK", body, kind)
        }
        _ => ("404 Not Found", b"not found".to_vec(), "text/plain"),
    };
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())?;
    stream.write_all(&body)
}

fn load(path: &Path) -> io::Result<Value> {
    let text = fs::read(path).map_err(|e| io::Error::other(format!("{}: {e}", path.display())))?;
    serde_json::from_slice(&text).map_err(|e| io::Error::other(format!("{}: {e}", path.display())))
}

fn write_json(path: &Path, value: &Value) -> io::Result<()> {
    fs::write(path, serde_json::to_vec(value)?)
}

/// `[covered lines, total lines, covered branches, total branches]`.
fn cov(entry: &Value) -> Value {
    let n = |kind: &str, field: &str| entry[kind][field].as_u64().unwrap_or(0);
    json!([
        n("lines", "covered"),
        n("lines", "total"),
        n("branches", "covered"),
        n("branches", "total")
    ])
}

/// Every file under `dir`, sorted, so a search finds the same one each time.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    for file in files_under(from) {
        let dest = to.join(file.strip_prefix(from).unwrap());
        fs::create_dir_all(dest.parent().unwrap())?;
        fs::copy(&file, dest)?;
    }
    Ok(())
}

/// `a/b/../c` as `a/c`: the crate pages link their sources relative to themselves.
fn normalise(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(p) => parts.push(p.to_string_lossy().into_owned()),
            _ => {}
        }
    }
    parts.join("/")
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// `[class, name, seconds, passed|failed|skipped]` for every test case.
fn junit(path: &Path) -> io::Result<Vec<Value>> {
    let text = fs::read_to_string(path)?;
    let doc = roxmltree::Document::parse(&text)
        .map_err(|e| io::Error::other(format!("{}: {e}", path.display())))?;
    Ok(doc
        .descendants()
        .filter(|n| n.has_tag_name("testcase"))
        .map(|case| {
            let has = |tag: &str| case.children().any(|c| c.has_tag_name(tag));
            let status = if has("failure") || has("error") {
                "failed"
            } else if has("skipped") {
                "skipped"
            } else {
                "passed"
            };
            let seconds: f64 = case
                .attribute("time")
                .and_then(|t| t.parse().ok())
                .unwrap_or(0.0);
            json!([
                case.attribute("classname").unwrap_or(""),
                case.attribute("name").unwrap_or(""),
                (seconds * 1000.0).round() / 1000.0,
                status
            ])
        })
        .collect())
}

/// The rows of an llvm-cov crate page: `(href, file, lines covered, lines, branches covered, branches)`.
fn crate_rows(page: &str) -> Vec<(String, String, u64, u64, u64, u64)> {
    let mut rows = Vec::new();
    for row in page.split("<tr><td><a href=\"").skip(1) {
        let parse = || -> Option<_> {
            let (href, rest) = row.split_once("\">")?;
            let (name, rest) = rest.split_once("</a></td><td>")?;
            let (lines, rest) = rest.split_once("</td><td>")?;
            let (branches, _) = rest.split_once("</td></tr>")?;
            let pair = |s: &str| -> Option<(u64, u64)> {
                let (a, b) = s.split_once('/')?;
                Some((a.parse().ok()?, b.parse().ok()?))
            };
            let (l1, l2) = pair(lines)?;
            let (b1, b2) = pair(branches)?;
            Some((href.to_string(), unescape(name), l1, l2, b1, b2))
        };
        rows.extend(parse());
    }
    rows
}

/// Coverage totals, per-crate and per-file Rust figures, and every test result.
fn coverage(src: &Path, out: &Path) -> io::Result<Value> {
    let run = load(&src.join("run.json"))?;
    let components: Map<String, Value> = run["components"]
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
    let (mut totals, mut tools) = (Map::new(), Map::new());
    for name in ["rust", "browser", "woocommerce"] {
        let manifest = src.join(format!("{name}.json"));
        if manifest.is_file() {
            let m = load(&manifest)?;
            totals.insert(name.into(), cov(&m));
            tools.insert(name.into(), m["tools"]["collector"].clone());
        }
    }
    let mut crates = Vec::new();
    if src.join("rust-crates.json").is_file() {
        for c in load(&src.join("rust-crates.json"))?
            .as_array()
            .into_iter()
            .flatten()
        {
            let report = c["report"].as_str().unwrap_or("");
            let page = fs::read_to_string(src.join(report)).unwrap_or_default();
            let base =
                Path::new("reports").join(Path::new(report).parent().unwrap_or(Path::new("")));
            let files: Vec<Value> = crate_rows(&page)
                .into_iter()
                .map(|(href, name, l1, l2, b1, b2)| {
                    json!([name, l1, l2, b1, b2, normalise(&base.join(href))])
                })
                .collect();
            let name = c["component"].as_str().unwrap_or("");
            crates.push(json!({
                "name": name, "cov": cov(c), "tool": TEST_TOOLS.contains(&name),
                "files": files,
                "unmeasured": if c["unavailable_files"].is_array() { c["unavailable_files"].clone() } else { json!([]) },
            }));
        }
    }
    let mut tests = json!({"rust": [], "browser": [], "woocommerce": []});
    if src.join("rust/junit.xml").is_file() {
        tests["rust"] = Value::from(junit(&src.join("rust/junit.xml"))?);
    }
    let mut browser = Vec::new();
    for (file, kind) in [
        ("junit-fixture.xml", "fixture"),
        ("junit-real-binaries.xml", "real binaries"),
    ] {
        let path = src.join("browser").join(file);
        if path.is_file() {
            for mut row in junit(&path)? {
                row.as_array_mut().unwrap().push(kind.into());
                browser.push(row);
            }
        }
    }
    tests["browser"] = Value::from(browser);
    if src.join("woocommerce/junit.xml").is_file() {
        tests["woocommerce"] = Value::from(junit(&src.join("woocommerce/junit.xml"))?);
    }
    let mut reports = Map::new();
    for name in ["rust", "browser", "woocommerce", "stress"] {
        if src.join(name).is_dir() {
            copy_tree(&src.join(name), &out.join("reports").join(name))?;
            if src.join(name).join("index.html").is_file() {
                reports.insert(name.into(), format!("reports/{name}/index.html").into());
            }
        }
    }
    let shipping = (0..4)
        .map(|i| {
            crates
                .iter()
                .filter(|c| c["tool"] == false)
                .map(|c| c["cov"][i].as_u64().unwrap_or(0))
                .sum::<u64>()
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "components": components, "revision": run["revision"], "totals": totals, "tools": tools,
        "crates": crates, "tests": tests, "reports": reports, "shipping": shipping,
    }))
}

/// The one-CPU stress run (CI) or the weekly scale run, as the page draws them.
fn stress(run_file: &Path) -> io::Result<Value> {
    let run = load(run_file)?;
    let scenario = &run["scenario"];
    let point = |result: &Value, name: Value| -> Value {
        let fx = &result["fixture"];
        let ticks = fx["points"].as_array().cloned().unwrap_or_default();
        let peak = result["peak_resident_bytes"].as_u64().unwrap_or_else(|| {
            ticks
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
        json!({
            "name": name, "status": result["status"], "detail": result["detail"], "tenants": tenants,
            "ticks": ticks.iter().map(|t| json!([t["phase"], t["duration_ms"], t["lagging_tenants"], t["oldest_lag_blocks"]])).collect::<Vec<_>>(),
            "peak_mb": if peak > 0 { json!((peak as f64 / 1048576.0 * 10.0).round() / 10.0) } else { Value::Null },
            "http_max_us": fx["http_max_latency_us"], "db_write_max_us": fx["db_write_query_max_us"],
            "rpc_calls": fx["rpc_calls"], "rpc_failures": fx["rpc_failures"], "rpc_delay_ms": fx["rpc_delay_ms"],
            "rpc_fail_every": fx["rpc_fail_every"], "rpc_fail_until_height": fx["rpc_fail_until_height"],
            "custody_slots": fx["custody_slots"], "custody_delay_ms": fx["custody_delay_ms"],
            "custody_scans": fx["custody_scans_completed"], "custody_max_wait_us": fx["custody_max_wait_us"],
            "lock_ms": fx["write_lock_hold_ms_per_tick"], "measured_ticks": fx["measured_ticks"], "drain_ticks": fx["drain_ticks"],
        })
    };
    let mut hardware = run["hardware"].clone();
    if !hardware.is_object() {
        // The run names the file it wrote the hardware to.
        let beside = run_file.with_file_name("hardware.json");
        hardware = if beside.is_file() {
            load(&beside)?
        } else {
            json!({})
        };
    }
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

/// The nightly property run of one build: its settings, scenario counts and every property.
fn properties(src: &Path, feature: &str) -> io::Result<Value> {
    let want = if feature == "default" { "" } else { feature };
    let files = files_under(src);
    let mut report = None;
    for path in files
        .iter()
        .filter(|p| p.ends_with("engine-exploration/properties/report.json"))
    {
        let r = load(path)?;
        if r["settings"]["ENGINE_FEATURES"].as_str().unwrap_or("") == want {
            report = Some(r);
            break;
        }
    }
    let Some(report) = report else {
        return Ok(Value::Null);
    };
    let tests = match files.iter().find(|p| p.ends_with("nextest/ci/junit.xml")) {
        Some(path) => junit(path)?,
        None => Vec::new(),
    };
    Ok(json!({
        "revision": report["revision"], "settings": report["settings"], "cases": report["semantic_cases"],
        "observations": report["semantic_observations"], "tests": tests,
    }))
}

/// Last night's campaign for each fuzz target of one build, deepest first.
fn fuzz(src: &Path, feature: &str) -> io::Result<Value> {
    let mut targets: BTreeMap<String, Value> = BTreeMap::new();
    for path in files_under(src)
        .iter()
        .filter(|p| p.ends_with("report.json"))
    {
        // .../engine-exploration/fuzz/<target>/<build>/<run>/<id>/report.json
        let parts: Vec<String> = path
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let n = parts.len();
        if n < 7
            || parts[n - 6] != "fuzz"
            || parts[n - 7] != "engine-exploration"
            || parts[n - 4] != feature
        {
            continue;
        }
        let target = parts[n - 5].clone();
        let r = load(path)?;
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
    let mut file = io::BufWriter::new(fs::File::create(path)?);
    image
        .to_rgb8()
        .write_with_encoder(JpegEncoder::new_with_quality(&mut file, quality))
        .map_err(io::Error::other)?;
    file.flush()
}

/// One screenshot as the viewer's full image and the grid's thumbnail.
fn convert(image: &Path, shape: &str, name: &str, out: &Path) -> io::Result<Value> {
    let full =
        image::open(image).map_err(|e| io::Error::other(format!("{}: {e}", image.display())))?;
    save_jpeg(&full, &out.join("gallery/full").join(name), 86)?;
    let width = match shape {
        "mobile-portrait" | "small-portrait" => 280,
        "element" => 360,
        _ => 480,
    }
    .min(full.width());
    let height = ((full.height() as u64 * width as u64) / full.width().max(1) as u64).max(1) as u32;
    let thumb = full.resize_exact(width, height, FilterType::CatmullRom);
    save_jpeg(&thumb, &out.join("gallery/thumb").join(name), 62)?;
    Ok(json!({
        "src": format!("gallery/thumb/{name}"), "w": thumb.width(), "h": thumb.height(),
        "full": format!("gallery/full/{name}"), "fw": full.width(), "fh": full.height(),
    }))
}

/// Every screenshot as a thumbnail for the grid and the full image for the viewer.
fn gallery(src: &Path, out: &Path) -> io::Result<Value> {
    let manifest = src.join("screenshots/manifest.json");
    if !manifest.is_file() {
        return Ok(Value::Null);
    }
    let shots = load(&manifest)?;
    let shots: Vec<&Value> = shots
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["retry"].as_u64().unwrap_or(0) == 0)
        .collect();
    fs::create_dir_all(out.join("gallery/full"))?;
    fs::create_dir_all(out.join("gallery/thumb"))?;
    let mut stages: Vec<Value> = Vec::new();
    let mut jobs: Vec<(usize, String, String, PathBuf, String)> = Vec::new();
    for shot in &shots {
        let (group, stage_name) = (
            shot["group"].as_str().unwrap_or(""),
            shot["stage"].as_str().unwrap_or(""),
        );
        let i = match stages
            .iter()
            .position(|s| s["group"] == group && s["stage"] == stage_name)
        {
            Some(i) => i,
            None => {
                stages.push(json!({
                    "group": group, "stage": stage_name, "test": shot["test"], "status": shot["status"],
                    // The manifest links the Playwright report relative to the screenshots folder.
                    "report": shot["report"].as_str().map(|r| r.strip_prefix("../").map_or(r.to_string(), |r| format!("reports/{r}"))),
                    "images": {}, "count": 0,
                }));
                stages.len() - 1
            }
        };
        let stage = &mut stages[i];
        stage["count"] = (stage["count"].as_u64().unwrap_or(0) + 1).into();
        if shot["status"] != "passed" {
            stage["status"] = shot["status"].clone();
        }
        let (shape, theme) = (
            shot["shape"].as_str().unwrap_or(""),
            shot["theme"].as_str().unwrap_or(""),
        );
        if !stage["images"][shape][theme].is_null() {
            continue;
        }
        if stage["images"][shape].is_null() {
            stage["images"][shape] = json!({});
        }
        // Filled in below, once the images are converted.
        stage["images"][shape][theme] = json!({});
        let image = src
            .join("screenshots")
            .join(shot["image"].as_str().unwrap_or(""));
        jobs.push((
            i,
            shape.to_string(),
            theme.to_string(),
            image,
            format!("{stage_name}-{shape}-{theme}.jpg"),
        ));
    }
    // Each screenshot converts on its own, so they share out over every core:
    // a CI run has hundreds.
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let converted: Vec<io::Result<Value>> = std::thread::scope(|scope| {
        let chunk = jobs.len().div_ceil(threads).max(1);
        let handles: Vec<_> = jobs
            .chunks(chunk)
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|(_, shape, _, image, name)| convert(image, shape, name, out))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a gallery thread panicked"))
            .collect()
    });
    for ((i, shape, theme, _, _), entry) in jobs.iter().zip(converted) {
        stages[*i]["images"][shape][theme] = entry?;
    }
    for stage in &mut stages {
        let mut shapes: Vec<String> = stage["images"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        shapes.sort_by_key(|s| SHAPES.iter().position(|k| k == s).unwrap_or(SHAPES.len()));
        stage["shapes"] = shapes.into();
    }
    stages.sort_by(|a, b| {
        (a["group"].as_str(), a["stage"].as_str()).cmp(&(b["group"].as_str(), b["stage"].as_str()))
    });
    Ok(json!({"stages": stages, "total": shots.len()}))
}

fn badge(totals: &Value) -> Value {
    let (covered, total) = (
        totals[0].as_u64().unwrap_or(0),
        totals[1].as_u64().unwrap_or(0),
    );
    if total == 0 {
        return json!({"schemaVersion": 1, "label": "coverage", "message": "unknown", "color": "lightgrey"});
    }
    let pct = 100.0 * covered as f64 / total as f64;
    let color = match pct {
        p if p >= 90.0 => "brightgreen",
        p if p >= 80.0 => "green",
        p if p >= 70.0 => "yellow",
        _ => "orange",
    };
    json!({"schemaVersion": 1, "label": "coverage", "message": format!("{pct:.1}%"), "color": color})
}

#[derive(Default)]
struct SiteArgs {
    out: Option<PathBuf>,
    coverage: Option<PathBuf>,
    properties: Option<PathBuf>,
    fuzz: Option<PathBuf>,
    scale: Option<PathBuf>,
    sources: Option<PathBuf>,
    feature: String,
    repo_url: String,
}

fn parse(args: &[&str]) -> io::Result<SiteArgs> {
    let mut parsed = SiteArgs {
        feature: "zmq".into(),
        repo_url: "https://github.com/OceanEilonwy/monokulo".into(),
        ..Default::default()
    };
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let value = rest
            .next()
            .ok_or_else(|| io::Error::other(format!("{flag} needs a value")))?;
        let path = Some(PathBuf::from(value));
        match *flag {
            "--out" => parsed.out = path,
            "--coverage" => parsed.coverage = path,
            "--properties" => parsed.properties = path,
            "--fuzz" => parsed.fuzz = path,
            "--scale" => parsed.scale = path,
            "--sources" => parsed.sources = path,
            "--feature" => parsed.feature = value.to_string(),
            "--repo-url" => parsed.repo_url = value.to_string(),
            _ => return Err(io::Error::other(format!("unknown option {flag}"))),
        }
    }
    Ok(parsed)
}

/// Builds the report into `--out`, replacing whatever was there.
pub(crate) fn site(root: &Path, args: &[&str]) -> io::Result<bool> {
    let args = parse(args)?;
    let out = args
        .out
        .ok_or_else(|| io::Error::other("pages build needs --out DIR"))?;
    if out.exists() {
        fs::remove_dir_all(&out)?;
    }
    fs::create_dir_all(&out)?;
    let mut data = json!({
        "feature": args.feature, "repo": args.repo_url,
        "sources": match &args.sources { Some(path) => load(path)?, None => json!({}) },
        "coverage": null, "stress": null, "gallery": null, "properties": null, "fuzz": null, "scale": null,
    });
    if let Some(src) = &args.coverage {
        data["coverage"] = coverage(src, &out)?;
        let run = src.join("stress/run.json");
        if run.is_file() {
            data["stress"] = stress(&run)?;
        }
        data["gallery"] = gallery(src, &out)?;
    }
    if let Some(src) = &args.properties {
        data["properties"] = properties(src, &args.feature)?;
    }
    if let Some(src) = &args.fuzz {
        data["fuzz"] = fuzz(src, &args.feature)?;
    }
    if let Some(src) = &args.scale {
        if let Some(run) = files_under(src)
            .into_iter()
            .find(|p| p.ends_with("run.json"))
        {
            data["scale"] = stress(&run)?;
        }
    }
    write_json(&out.join("data.json"), &data)?;
    write_json(
        &out.join("badge.json"),
        &badge(&data["coverage"]["shipping"]),
    )?;
    fs::create_dir_all(out.join("assets"))?;
    fs::copy(
        root.join("crates/monokulo/src/views/theme.css"),
        out.join("assets/theme.css"),
    )?;
    for weight in [500, 700, 800] {
        let font = format!("manrope-{weight}.woff2");
        fs::copy(
            root.join("crates/monokulo/static").join(&font),
            out.join("assets").join(&font),
        )?;
    }
    let page = fs::read_to_string(root.join("web/pages/quality/index.html"))?;
    fs::write(
        out.join("index.html"),
        page.replace("@REPO_URL@", &args.repo_url),
    )?;
    fs::write(out.join(".nojekyll"), "")?;
    let have: Vec<&str> = ["coverage", "properties", "fuzz", "scale"]
        .into_iter()
        .filter(|k| !data[*k].is_null())
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
    Ok(true)
}

/// What the Pages site is built from: `(key, workflow file, artifact name test, required, needs success)`.
/// The site is required because Pages serves one deployment, so publishing
/// without it would take the package repository offline.
type Source = (&'static str, &'static str, fn(&str) -> bool, bool, bool);
const SOURCES: [Source; 5] = [
    (
        "site",
        "openwrt.yml",
        |n| n == "monokulo-openwrt-site",
        true,
        true,
    ),
    (
        "coverage",
        "ci.yml",
        |n| {
            n.strip_prefix("coverage-")
                .is_some_and(|sha| sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
        },
        false,
        false,
    ),
    (
        "properties",
        "engine-properties.yml",
        |n| n.starts_with("engine-properties-zmq-"),
        false,
        false,
    ),
    (
        "fuzz",
        "engine-fuzz.yml",
        |n| n.starts_with("engine-fuzz-") && n.rsplit('-').nth(1) == Some("zmq"),
        false,
        false,
    ),
    (
        "scale",
        "engine-scale.yml",
        |n| n.starts_with("engine-scale-measurements-"),
        false,
        false,
    ),
];
const RUNS_TO_SEARCH: u32 = 30;

fn gh(args: &[&str]) -> io::Result<String> {
    let output = Command::new("gh")
        .args(args)
        .output()
        .map_err(|e| io::Error::other(format!("missing prerequisite: gh ({e})")))?;
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
    serde_json::from_str(&gh(&["api", &format!("repos/{repo}/{path}")])?).map_err(io::Error::other)
}

/// Downloads main's newest artifacts of each source into `dir`. For each it
/// takes the newest finished run that still has them, skipping cancelled
/// runs (a failed run's results are shown, not hidden), and writes
/// `dir/sources.json` naming the run behind each.
pub(crate) fn pages_inputs(args: &[&str]) -> io::Result<bool> {
    let (dir, repo) = match args {
        [dir] => (PathBuf::from(dir), env::var("GITHUB_REPOSITORY").ok()),
        [dir, "--repo", repo] => (PathBuf::from(dir), Some(repo.to_string())),
        _ => {
            return Err(io::Error::other(
                "usage: cargo xtask pages fetch DIR [--repo OWNER/NAME]",
            ))
        }
    };
    let repo = repo
        .ok_or_else(|| io::Error::other("name the repository with --repo or GITHUB_REPOSITORY"))?;
    let mut sources = Map::new();
    for (key, workflow, wanted, required, need_success) in SOURCES {
        let runs = api(&repo, &format!("actions/workflows/{workflow}/runs?branch=main&status=completed&per_page={RUNS_TO_SEARCH}"))?;
        let mut found = None;
        for run in runs["workflow_runs"].as_array().into_iter().flatten() {
            let conclusion = run["conclusion"].as_str().unwrap_or("");
            if conclusion == "cancelled"
                || conclusion == "skipped"
                || (need_success && conclusion != "success")
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
                .filter(|n| wanted(n))
                .map(str::to_string)
                .collect();
            if !names.is_empty() {
                found = Some((run.clone(), names));
                break;
            }
        }
        let Some((run, names)) = found else {
            if required {
                return Err(io::Error::other(format!(
                    "{key}: no run of {workflow} on main in the last {RUNS_TO_SEARCH} still has its artifact; \
                     run that workflow on main (Actions > Run workflow), then this one again"
                )));
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
    fs::create_dir_all(&dir)?;
    fs::write(
        dir.join("sources.json"),
        serde_json::to_vec_pretty(&sources)?,
    )?;
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
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    const SHA: &str = "7e8f34dac14b985bd24a323147b71a10e2bc4b05";

    /// A folder of its own under the system temp dir, removed when dropped.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = env::temp_dir().join(format!("xtask-quality-{name}-{}", std::process::id()));
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

    fn put(path: &Path, text: impl AsRef<[u8]>) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn put_json(path: &Path, value: Value) {
        put(path, serde_json::to_vec(&value).unwrap());
    }

    fn junit_xml(cases: &[(&str, &str, f64, &str)]) -> String {
        let rows: String = cases
            .iter()
            .map(|(class, name, time, extra)| format!(r#"<testcase name="{name}" classname="{class}" time="{time}">{extra}</testcase>"#))
            .collect();
        format!("<testsuites><testsuite>{rows}</testsuite></testsuites>")
    }

    fn totals(l: u64, lt: u64, b: u64, bt: u64) -> Value {
        json!({"lines": {"covered": l, "total": lt}, "branches": {"covered": b, "total": bt}})
    }

    fn merged(mut a: Value, b: Value) -> Value {
        a.as_object_mut()
            .unwrap()
            .extend(b.as_object().unwrap().clone());
        a
    }

    /// A cut-down coverage-<sha> artifact: two crates, three suites, the stress run, two screenshots.
    fn coverage_artifact(root: &Path) {
        put_json(
            &root.join("run.json"),
            json!({"revision": SHA, "components": [{"component": "rust", "status": "passed"}]}),
        );
        put_json(
            &root.join("rust.json"),
            merged(
                totals(90, 100, 8, 10),
                json!({"tools": {"collector": "cargo-llvm-cov 0.9.1"}}),
            ),
        );
        put_json(
            &root.join("browser.json"),
            merged(
                totals(19, 20, 9, 10),
                json!({"tools": {"collector": "istanbul"}}),
            ),
        );
        put_json(
            &root.join("rust-crates.json"),
            json!([
                merged(
                    totals(80, 90, 8, 9),
                    json!({"component": "engine", "report": "rust/crates/engine.html", "unavailable_files": ["crates/engine/src/lib.rs"]})
                ),
                merged(
                    totals(10, 40, 0, 1),
                    json!({"component": "cli-wallet", "report": "rust/crates/cli-wallet.html", "unavailable_files": []})
                ),
            ]),
        );
        put(
            &root.join("rust/crates/engine.html"),
            r#"<table><tr><th>Source</th></tr><tr><td><a href="../coverage/home/runner/crates/engine/src/scanner.rs.html">src/scanner.rs</a></td><td>80/90</td><td>8/9</td></tr></table>"#,
        );
        put(&root.join("rust/crates/cli-wallet.html"), "<table></table>");
        put(&root.join("rust/index.html"), "<html></html>");
        put(
            &root.join("rust/coverage/home/runner/crates/engine/src/scanner.rs.html"),
            "<html></html>",
        );
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
        put(&root.join("browser/index.html"), "<html></html>");
        let tick = |phase: &str, ms: u64| json!({"phase": phase, "duration_ms": ms, "lagging_tenants": 0, "oldest_lag_blocks": 0});
        let point = |tenants: u64, ms: u64| {
            json!({"tenants": tenants, "status": "sustainable", "peak_resident_bytes": 24 * 1048576, "fixture": {
                "tenants": tenants, "http_max_latency_us": 5000, "measured_ticks": 2, "drain_ticks": 1,
                "points": [tick("warmup", 90), tick("measured", ms), tick("measured", ms)]}})
        };
        let mut fault_tick = tick("measured", 70);
        fault_tick["process_memory"] = json!({"peak_resident_bytes": 1048576});
        put_json(
            &root.join("stress/run.json"),
            json!({"profile": "ci", "hardware": "hardware.json",
                "scenario": {"poll_interval_ms": 5000, "max_oldest_lag_blocks": 3, "max_http_latency_ms": 250},
                "results": [point(32, 58), point(128, 107)],
                "faults": [{"file": "fault-rpc", "status": "recovered", "tenants": 16, "fixture": {
                    "rpc_calls": 70, "rpc_failures": 6, "rpc_delay_ms": 2, "rpc_fail_every": 3, "rpc_fail_until_height": 2,
                    "measured_ticks": 2, "drain_ticks": 6, "points": [fault_tick]}}]}),
        );
        put_json(
            &root.join("stress/hardware.json"),
            json!({"cpu_model": "Test CPU", "effective_cores": 4, "sqlite_version": "3.53.2", "selected_cpu": 0}),
        );
        put(&root.join("stress/index.html"), "<html></html>");
        let shots = root.join("screenshots");
        fs::create_dir_all(shots.join("images")).unwrap();
        RgbImage::from_pixel(1280, 800, Rgb([255, 255, 255]))
            .save(shots.join("images/checkout-paid.png"))
            .unwrap();
        RgbImage::from_pixel(390, 844, Rgb([255, 255, 255]))
            .save(shots.join("images/checkout-paid-phone.png"))
            .unwrap();
        let shot = |shape: &str, image: &str, retry: u64| {
            json!({"group": "checkout", "stage": "checkout-paid", "test": "checkout shows paid", "status": "passed", "retry": retry,
                "report": "../browser/playwright-report/index.html", "shape": shape, "theme": "light", "image": image})
        };
        put_json(
            &shots.join("manifest.json"),
            json!([
                shot("desktop", "images/checkout-paid.png", 0),
                shot("mobile-portrait", "images/checkout-paid-phone.png", 0),
                shot("desktop", "images/checkout-paid.png", 1),
            ]),
        );
    }

    /// Property and fuzz reports for both engine builds, as their artifacts lay them out.
    fn exploration_artifacts(root: &Path) {
        for (feature, cases) in [("zmq", 1114), ("", 900)] {
            let build = if feature.is_empty() {
                "default"
            } else {
                feature
            };
            put_json(
                &root.join(format!(
                    "properties-{build}/target/engine-exploration/properties/report.json"
                )),
                json!({"revision": SHA, "settings": {"PROPTEST_CASES": "512", "ENGINE_FEATURES": feature},
                    "semantic_cases": cases, "semantic_observations": {"sql-denial-reached": 12515}}),
            );
            put(
                &root.join(format!("properties-{build}/target/nextest/ci/junit.xml")),
                junit_xml(&[(
                    "engine",
                    "work::tests::properties::money_matches_the_model",
                    900.0,
                    "",
                )]),
            );
            for (target, edges) in [("portfolio", 28476), ("inputs", 6355)] {
                put_json(
                    &root.join(format!("fuzz/engine-fuzz-{target}-{build}-1/target/engine-exploration/fuzz/{target}/{build}/1/abc/report.json")),
                    json!({"status": "passed", "wall_seconds": 400.4, "corpus": {"files": 9}, "new_unique_inputs": 3,
                        "exploration": {"final": {"coverage": edges + if feature.is_empty() { 1 } else { 0 }, "executions": 724}, "coverage_growth": 35},
                        "semantic_cases": 2, "semantic_observations": {}}),
                );
            }
        }
    }

    struct Fixture {
        scratch: Scratch,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let scratch = Scratch::new(name);
            coverage_artifact(&scratch.0.join("coverage"));
            exploration_artifacts(&scratch.0.join("runs"));
            Fixture { scratch }
        }
        fn path(&self, rel: &str) -> String {
            self.scratch.0.join(rel).to_string_lossy().into_owned()
        }
        fn out(&self) -> PathBuf {
            self.scratch.0.join("site/quality")
        }
        fn build(&self, args: &[&str]) -> Value {
            let out = self.path("site/quality");
            let mut all = vec!["--out", out.as_str()];
            all.extend_from_slice(args);
            assert!(site(&crate::root(), &all).unwrap());
            load(&self.out().join("data.json")).unwrap()
        }
    }

    #[test]
    fn coverage_links_each_file_to_its_annotated_source() {
        let f = Fixture::new("links");
        let data = f.build(&["--coverage", &f.path("coverage")]);
        let engine = data["coverage"]["crates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "engine")
            .unwrap()
            .clone();
        let file = &engine["files"][0];
        assert_eq!(
            (file[0].as_str(), file[1].as_u64(), file[2].as_u64()),
            (Some("src/scanner.rs"), Some(80), Some(90))
        );
        let link = file[5].as_str().unwrap();
        assert_eq!(
            link,
            "reports/rust/coverage/home/runner/crates/engine/src/scanner.rs.html"
        );
        assert!(f.out().join(link).is_file());
        assert_eq!(engine["unmeasured"], json!(["crates/engine/src/lib.rs"]));
    }

    #[test]
    fn test_tools_stay_out_of_the_shipping_figure_and_the_badge() {
        let f = Fixture::new("badge");
        let data = f.build(&["--coverage", &f.path("coverage")]);
        assert_eq!(data["coverage"]["shipping"], json!([80, 90, 8, 9]));
        let wallet = data["coverage"]["crates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "cli-wallet")
            .unwrap()
            .clone();
        assert_eq!(wallet["tool"], true);
        assert_eq!(
            load(&f.out().join("badge.json")).unwrap()["message"],
            "88.9%"
        );
    }

    #[test]
    fn every_test_keeps_its_result() {
        let f = Fixture::new("results");
        let data = f.build(&["--coverage", &f.path("coverage")]);
        let statuses: Vec<&str> = data["coverage"]["tests"]["rust"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r[3].as_str().unwrap())
            .collect();
        assert_eq!(statuses, ["passed", "failed", "skipped"]);
        assert_eq!(data["coverage"]["tests"]["browser"][0][4], "fixture");
    }

    #[test]
    fn stress_run_reads_its_hardware_file_and_fault_settings() {
        let f = Fixture::new("stress");
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
            json!(["measured", 107, 0, 0])
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
    fn only_the_named_build_of_the_nightly_runs_is_shown() {
        let f = Fixture::new("builds");
        let data = f.build(&["--properties", &f.path("runs"), "--fuzz", &f.path("runs")]);
        assert_eq!(data["properties"]["cases"], 1114);
        let targets: Vec<&str> = data["fuzz"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["target"].as_str().unwrap())
            .collect();
        assert_eq!(targets, ["portfolio", "inputs"]);
        assert_eq!(data["fuzz"][0]["edges"], 28476);
        let default = f.build(&["--properties", &f.path("runs"), "--feature", "default"]);
        assert_eq!(default["properties"]["cases"], 900);
    }

    #[test]
    fn gallery_keeps_a_thumbnail_and_the_full_image_of_each_first_try() {
        let f = Fixture::new("gallery");
        let gallery = f.build(&["--coverage", &f.path("coverage")])["gallery"].clone();
        assert_eq!(gallery["total"], 2);
        let stage = &gallery["stages"][0];
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
    }

    #[test]
    fn missing_inputs_leave_their_sections_out_but_still_build_the_page() {
        let f = Fixture::new("empty");
        let data = f.build(&[]);
        for key in ["coverage", "properties", "fuzz", "scale"] {
            assert!(data[key].is_null(), "{key}");
        }
        assert_eq!(
            load(&f.out().join("badge.json")).unwrap()["message"],
            "unknown"
        );
        assert!(!fs::read_to_string(f.out().join("index.html"))
            .unwrap()
            .contains("@REPO_URL@"));
        assert!(f.out().join("assets/theme.css").is_file());
    }

    #[test]
    fn a_rebuild_starts_clean() {
        let f = Fixture::new("rebuild");
        f.build(&["--coverage", &f.path("coverage")]);
        f.build(&[]);
        assert!(!f.out().join("reports").exists());
    }

    #[test]
    fn serve_answers_files_and_refuses_paths_outside_its_folder() {
        use std::io::Read;
        let scratch = Scratch::new("serve");
        put(&scratch.0.join("site/index.html"), "<p>report</p>");
        put(&scratch.0.join("secret.txt"), "outside");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let site = scratch.0.join("site");
        let server = std::thread::spawn(move || {
            for stream in listener.incoming().take(3) {
                respond(&site, stream.unwrap()).unwrap();
            }
        });
        let get = |path: &str| {
            let mut stream = std::net::TcpStream::connect(addr).unwrap();
            write!(stream, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
            let mut reply = String::new();
            stream.read_to_string(&mut reply).unwrap();
            reply
        };
        let page = get("/");
        assert!(
            page.starts_with("HTTP/1.1 200")
                && page.contains("text/html")
                && page.ends_with("<p>report</p>"),
            "{page}"
        );
        assert!(get("/missing.json").starts_with("HTTP/1.1 404"));
        assert!(get("/../secret.txt").starts_with("HTTP/1.1 404"));
        server.join().unwrap();
    }

    #[test]
    fn the_sources_pick_only_the_artifacts_the_report_reads() {
        let pick = |key: &str, name: &str| (SOURCES.iter().find(|s| s.0 == key).unwrap().2)(name);
        assert!(pick("coverage", &format!("coverage-{SHA}")));
        assert!(!pick("coverage", &format!("coverage-rust-{SHA}")));
        assert!(pick("fuzz", "engine-fuzz-portfolio-zmq-37702639422"));
        assert!(!pick("fuzz", "engine-fuzz-portfolio-default-37702639422"));
        assert!(pick("properties", "engine-properties-zmq-37695839811"));
        assert!(!pick("properties", "engine-properties-default-37695839811"));
    }
}
