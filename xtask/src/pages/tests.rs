//! The report built from cut-down artifacts laid out as CI leaves them.

use super::build;
use super::inputs::{self, Shape, TestStatus, Theme};
use crate::exploration::Build;
use crate::support::{files_under, html_links, read_json, Scratch};
use image::{Rgb, RgbImage};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};

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
        .map(|(class, name, time, extra)| {
            format!(
                r#"<testcase name="{name}" classname="{class}" time="{time}">{extra}</testcase>"#
            )
        })
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
    // One Playwright run of both projects, each spec's testsuite naming its
    // project as the hostname.
    put(
        &root.join("browser/junit.xml"),
        r#"<testsuites>
            <testsuite name="checkout.spec.js" hostname="fixture"><testcase name="checkout shows the amount" classname="checkout.spec.js" time="2"/></testsuite>
            <testsuite name="logs.spec.js" hostname="real-binaries"><testcase name="logs load" classname="logs.spec.js" time="3"/></testsuite>
        </testsuites>"#,
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

    fn build(&self, args: &[&str]) {
        let out = self.path("site/quality");
        let mut all = vec!["--out", out.as_str()];
        all.extend_from_slice(args);
        assert!(build(&crate::root(), &all).unwrap().succeeded());
    }

    fn page(&self, name: &str) -> String {
        fs::read_to_string(self.out().join(name)).unwrap()
    }

    /// Every file the build wrote under `dir`, relative to the site.
    fn files(&self, dir: &str) -> Vec<String> {
        files_under(&self.out().join(dir), |_| false)
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

fn page_names(out: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(out)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".html"))
        .collect();
    names.sort();
    names
}

#[test]
fn coverage_links_each_file_to_its_annotated_source() {
    let f = Fixture::new();
    f.build(&["--coverage", &f.path("coverage")]);
    let page = f.page("coverage.html");
    let link = "reports/rust/coverage/home/runner/crates/engine/src/scanner.rs.html";
    assert!(page.contains(&format!("href=\"{link}\"")), "{page}");
    assert!(f.out().join(link).is_file());
    // The file the profile couldn't instrument is listed, relative to its crate.
    assert!(page.contains("<td>src/lib.rs</td>"), "{page}");
    // There's no WooCommerce manifest, so there is no WooCommerce row or figure.
    assert!(page.contains(">Browser</a>"));
    assert!(!page.contains("WooCommerce</dt>"));
    assert!(f.page("index.html").contains("not measured in this run"));
}

#[test]
fn only_the_linked_report_files_and_what_they_need_ship() {
    let f = Fixture::new();
    f.build(&["--coverage", &f.path("coverage")]);
    assert_eq!(
        f.files("reports"),
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
fn every_local_link_of_every_page_resolves() {
    let f = Fixture::new();
    f.build(&[
        "--coverage",
        &f.path("coverage"),
        "--properties",
        &f.path("runs"),
        "--fuzz",
        &f.path("runs"),
    ]);
    let pages = page_names(&f.out());
    assert_eq!(
        pages,
        [
            "coverage.html",
            "fuzzing.html",
            "index.html",
            "load.html",
            "properties.html",
            "screens.html",
            "tests.html"
        ]
    );
    for name in &pages {
        let page = f.page(name);
        let mut links = html_links(&page);
        // The gallery's full-size images are in the viewer's data, not in markup.
        links.extend(
            page.match_indices("\"full\":\"")
                .map(|(at, m)| page[at + m.len()..].split('"').next().unwrap().to_string()),
        );
        for link in links {
            // The site's own front page and the repository are outside the report.
            if link.starts_with("../") || link.starts_with("https://") || link == "./" {
                continue;
            }
            assert!(
                f.out().join(&link).is_file(),
                "{name} links {link}, which isn't there"
            );
        }
    }
}

#[test]
fn test_tools_stay_out_of_the_shipping_figure_and_the_badge() {
    let f = Fixture::new();
    let (run, _) = inputs::coverage(&f.scratch.join("coverage")).unwrap();
    assert_eq!(
        (run.shipping.lines.covered, run.shipping.lines.total),
        (80, 90)
    );
    let wallet = run.crates.iter().find(|c| c.name == "cli-wallet").unwrap();
    assert!(wallet.tool);
    f.build(&["--coverage", &f.path("coverage")]);
    let badge: serde_json::Value = read_json(&f.out().join("badge.json")).unwrap();
    assert_eq!(badge["message"], "88.9%");
    assert_eq!(badge["color"], "green");
    assert_eq!(badge["schemaVersion"], 1);
}

#[test]
fn every_test_keeps_its_result_and_is_listed_without_javascript() {
    let f = Fixture::new();
    let (run, _) = inputs::coverage(&f.scratch.join("coverage")).unwrap();
    let statuses: Vec<TestStatus> = run.tests.rust.iter().map(|t| t.status).collect();
    assert_eq!(
        statuses,
        [TestStatus::Passed, TestStatus::Failed, TestStatus::Skipped]
    );
    let kinds: Vec<Option<inputs::BrowserKind>> =
        run.tests.browser.iter().map(|t| t.kind).collect();
    assert_eq!(
        kinds,
        [
            Some(inputs::BrowserKind::Fixture),
            Some(inputs::BrowserKind::RealBinaries)
        ]
    );
    f.build(&["--coverage", &f.path("coverage")]);
    let page = f.page("tests.html");
    assert_eq!(page.matches("<li data-suite=").count(), 5);
    assert!(page.contains(">logs (real binaries)<"));
    assert!(page.contains("A reorg moves the payment"));
    assert!(page.contains(">checkout<"));
    // A failed test means the run line doesn't claim everything passed.
    assert!(page.contains("Some checks failed"));
}

#[test]
fn artifact_text_is_escaped_everywhere_it_is_shown() {
    let f = Fixture::new();
    let hostile = "<img src=x onerror=alert(1)>\"'";
    put(
        &f.scratch.join("coverage/woocommerce/junit.xml"),
        format!(
            r#"<testsuites><testcase classname="Plugin&lt;/script&gt;" name="{}" time="1"/></testsuites>"#,
            hostile
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
        ),
    );
    let manifest = f.scratch.join("coverage/screenshots/manifest.json");
    let mut shots: serde_json::Value = read_json(&manifest).unwrap();
    shots[0]["test"] = hostile.into();
    shots[0]["stage"] = "</script><script>alert(1)</script>".into();
    put_json(&manifest, &shots);
    f.build(&["--coverage", &f.path("coverage")]);
    for name in page_names(&f.out()) {
        let page = f.page(&name);
        assert!(!page.contains("<img src=x"), "{name}");
        assert!(!page.contains("</script><script>"), "{name}");
        assert!(!page.contains("Plugin</script>"), "{name}");
    }
    let screens = f.page("screens.html");
    let data = screens
        .split("<script id=\"screens-data\" type=\"application/json\">")
        .nth(1)
        .unwrap();
    let data = data.split("</script>").next().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(data).unwrap();
    assert!(parsed
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["test"] == hostile));
}

#[test]
fn the_stress_run_reads_its_named_hardware_file_and_fault_settings() {
    let f = Fixture::new();
    let run = inputs::stress(&f.scratch.join("coverage/stress/run.json")).unwrap();
    assert_eq!(run.scenario.budget_ms, Some(5000));
    assert_eq!(run.hardware.cpu_model.as_deref(), Some("Test CPU"));
    let tenants: Vec<Option<u64>> = run
        .points
        .iter()
        .map(inputs::StressPoint::tenants)
        .collect();
    assert_eq!(tenants, [Some(32), Some(128)]);
    let rpc = &run.faults[0];
    assert_eq!(rpc.fault(), Some(inputs::Fault::Rpc));
    assert_eq!(
        (rpc.fixture.rpc_fail_every, rpc.fixture.rpc_failures),
        (Some(3), Some(6))
    );
    assert_eq!(rpc.peak_bytes(), Some(1_048_576));
    f.build(&["--coverage", &f.path("coverage")]);
    let page = f.page("load.html");
    assert!(page.contains("Test CPU"));
    assert!(page.contains("every third request failed until block 2"));
    assert!(page.contains("<svg"));
}

#[test]
fn only_the_named_build_of_the_nightly_runs_is_shown_with_its_own_tests() {
    let f = Fixture::new();
    let runs = f.scratch.join("runs");
    let zmq = inputs::properties(&runs, Build::Zmq).unwrap().unwrap();
    assert_eq!(zmq.cases, 1114);
    assert_eq!(
        zmq.tests[0].name,
        "work::tests::properties::money_matches_the_model_zmq"
    );
    let default = inputs::properties(&runs, Build::Default).unwrap().unwrap();
    assert_eq!(default.cases, 900);
    assert_eq!(
        default.tests[0].name,
        "work::tests::properties::money_matches_the_model_default"
    );
    let targets: Vec<(String, Option<u64>)> = inputs::fuzz(&runs, Build::Zmq)
        .unwrap()
        .into_iter()
        .map(|t| (t.target, t.edges))
        .collect();
    assert_eq!(
        targets,
        [
            ("portfolio".into(), Some(28476)),
            ("inputs".into(), Some(6355))
        ]
    );
    assert_eq!(
        inputs::fuzz(&runs, Build::Default).unwrap()[0].edges,
        Some(28477)
    );
    f.build(&[
        "--properties",
        &f.path("runs"),
        "--fuzz",
        &f.path("runs"),
        "--feature",
        "default",
    ]);
    assert!(f
        .page("properties.html")
        .contains("Money matches the model default"));
}

#[test]
fn gallery_keeps_a_thumbnail_and_the_full_image_of_each_first_try() {
    let f = Fixture::new();
    let out = f.out();
    fs::create_dir_all(&out).unwrap();
    let (gallery, _) = inputs::gallery(&f.scratch.join("coverage"), &out)
        .unwrap()
        .unwrap();
    assert_eq!(gallery.total, 3);
    let checkout = &gallery.screens[0];
    assert_eq!(checkout.group, "checkout");
    let shapes: Vec<Shape> = checkout.shapes().collect();
    assert_eq!(shapes, [Shape::Desktop, Shape::MobilePortrait]);
    let desktop = checkout.image(Shape::Desktop, Theme::Light).unwrap();
    assert_eq!((desktop.w, desktop.fw), (480, 1280));
    // Only a light shot was taken, so the dark one falls back to it.
    assert_eq!(
        checkout.image(Shape::Desktop, Theme::Dark).unwrap().full,
        desktop.full
    );
    assert_eq!(
        checkout
            .image(Shape::MobilePortrait, Theme::Light)
            .unwrap()
            .w,
        280
    );
    assert!(out.join(&desktop.full).is_file());
    assert_eq!(
        checkout.report.as_deref(),
        Some("reports/browser/playwright-report/index.html")
    );
    // The same stage name in another group has images of its own.
    let pos = &gallery.screens[1];
    assert_eq!(pos.group, "pos");
    let pos_full = &pos.image(Shape::Desktop, Theme::Light).unwrap().full;
    assert_ne!(pos_full, &desktop.full);
    assert!(out.join(pos_full).is_file());
}

#[test]
fn missing_inputs_leave_their_sections_out_but_still_build_the_front_page() {
    let f = Fixture::new();
    f.build(&[]);
    assert_eq!(page_names(&f.out()), ["index.html"]);
    let page = f.page("index.html");
    assert!(page.contains("No test results have been published yet."));
    assert!(!page.contains("class=\"tabs\""));
    let badge: serde_json::Value = read_json(&f.out().join("badge.json")).unwrap();
    assert_eq!(badge["message"], "unknown");
    for asset in [
        "theme.css",
        "quality.css",
        "quality.js",
        "manrope-500.woff2",
    ] {
        assert!(f.out().join("assets").join(asset).is_file(), "{asset}");
    }
    // Fuzzing alone still gets the front page, which says what it lacks.
    f.build(&["--fuzz", &f.path("runs")]);
    assert_eq!(page_names(&f.out()), ["fuzzing.html", "index.html"]);
    assert!(f
        .page("index.html")
        .contains("This run published no test results."));
}

#[test]
fn a_rebuild_replaces_its_own_files_and_nothing_else() {
    let f = Fixture::new();
    f.build(&["--coverage", &f.path("coverage")]);
    put(&f.out().join("somebody-elses.txt"), "kept");
    f.build(&[]);
    assert!(!f.out().join("reports").exists());
    assert!(!f.out().join("gallery").exists());
    assert!(!f.out().join("tests.html").exists());
    assert!(f.out().join("somebody-elses.txt").is_file());
}

#[test]
fn only_the_pages_with_filters_load_the_script() {
    let f = Fixture::new();
    f.build(&["--coverage", &f.path("coverage")]);
    for name in page_names(&f.out()) {
        let loads = f.page(&name).contains("assets/quality.js");
        assert_eq!(
            loads,
            name == "tests.html" || name == "screens.html",
            "{name}"
        );
    }
}

#[test]
fn manifest_paths_cannot_reach_outside_the_artifact() {
    let f = Fixture::new();
    put(
        &f.scratch.join("outside.html"),
        "<p>not part of the report</p>",
    );
    let manifest = f.scratch.join("coverage/screenshots/manifest.json");
    let mut shots: Value = read_json(&manifest).unwrap();
    shots[0]["report"] = "../../outside.html".into();
    put_json(&manifest, &shots);
    f.build(&["--coverage", &f.path("coverage")]);
    assert!(!f.scratch.join("site/outside.html").exists());
    assert!(!f.files("reports").iter().any(|p| p.contains("outside")));
    // Nor does any page link it.
    for name in page_names(&f.out()) {
        let page = f.page(&name);
        assert!(!page.contains("reports/.."), "{name}");
        assert!(!page.contains("outside.html"), "{name}");
    }

    shots[0]["image"] = "../../outside.html".into();
    put_json(&manifest, &shots);
    let out = f.path("site/quality");
    let error = build(
        &crate::root(),
        &["--out", &out, "--coverage", &f.path("coverage")],
    )
    .expect_err("a screenshot outside the artifact is refused");
    assert!(
        error.to_string().contains("outside the screenshots folder"),
        "{error}"
    );
}

#[test]
fn a_shape_taken_in_one_theme_is_shown_once_and_named_for_that_theme() {
    let f = Fixture::new();
    f.build(&["--coverage", &f.path("coverage")]);
    let page = f.page("screens.html");
    // The fixture's shots are all light: no dark picture stands in for one.
    assert!(!page.contains("class=\"dark\""), "{page}");
    assert!(!page.contains(", dark\""), "{page}");
    assert!(page.contains("class=\"light\""));
}

#[test]
fn a_shot_without_a_result_keeps_an_earlier_failure() {
    let f = Fixture::new();
    let manifest = f.scratch.join("coverage/screenshots/manifest.json");
    let mut shots: Value = read_json(&manifest).unwrap();
    shots[0]["status"] = "failed".into();
    shots[1].as_object_mut().unwrap().remove("status");
    put_json(&manifest, &shots);
    let out = f.out();
    fs::create_dir_all(&out).unwrap();
    let (gallery, _) = inputs::gallery(&f.scratch.join("coverage"), &out)
        .unwrap()
        .unwrap();
    assert_eq!(gallery.screens[0].status.as_deref(), Some("failed"));
}
