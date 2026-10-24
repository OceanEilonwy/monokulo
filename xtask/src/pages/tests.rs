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

/// A live-network artifact as `cargo xtask live` leaves it: both `JUnit`
/// reports and live.json. The mainnet node was down, so its test's failure
/// is the node's; the DNS test failed with every service answering.
fn live_artifact(root: &Path) {
    put(
        &root.join("rust-junit.xml"),
        junit_xml(&[
            (
                "engine",
                "daemon_rpc::live_node_tests::live_node_get_height_returns_a_plausible_value",
                1.5,
                r#"<failure message="connection refused">error sending request</failure>"#,
            ),
            (
                "monokulo",
                "embed_domains::tests::live_system_resolver_reads_real_txt_records",
                0.2,
                r#"<failure message="got: [&lt;script&gt;]"/>"#,
            ),
            (
                "shared",
                "haveno::tests::live_haveno_markets_api_prices_fiat_under_both_policies",
                3.0,
                "",
            ),
            (
                "shared",
                "coinmarketcap::tests::live_coinmarketcap_api_quotes_usd",
                31.0,
                "",
            ),
        ]),
    );
    put(
        &root.join("browser-junit.xml"),
        junit_xml(&[(
            "pos-stagenet-payments.spec.js",
            "a 0-conf-trusted payment shows shared success and stays reviewable",
            90.0,
            "",
        )]),
    );
    put_json(
        &root.join("live.json"),
        &json!({
            "services": [
                {"id": "mainnet-node", "name": "Monero mainnet node", "reachable": false, "detail": "https://node.example:18089/get_height: Connection refused"},
                {"id": "stagenet-node", "name": "Monero stagenet nodes", "reachable": true, "detail": "answered HTTP 200"},
                {"id": "dns", "name": "DNS", "reachable": true, "detail": "google.com is 192.0.2.1"},
                {"id": "haveno", "name": "haveno.markets", "reachable": true, "detail": "answered HTTP 200"},
                {"id": "wallets", "name": "Funded stagenet test wallets", "reachable": true, "detail": "the spender can pay a run"},
                {"id": "tor", "name": "Tor network", "reachable": null, "detail": "only the test itself can tell whether it works"}
            ],
            "wallets": {
                "dir": "e2e/wallets/ci",
                "before": [{"role": "spender", "address": "53etP59", "balance": 60_000_000_000u64, "unlocked": 60_000_000_000u64, "outputs": 16, "ready": 16}],
                "after": [
                    {"role": "spender", "address": "53etP59", "balance": 59_000_000_000u64, "unlocked": 50_000_000_000u64, "outputs": 17, "ready": 10},
                    {"role": "merchant", "address": "52Ed34j", "balance": 670_000_000u64, "unlocked": 0, "outputs": 2, "ready": 0}
                ],
                "problem": null,
                "kept": ["Swept 0.000670000000 XMR from the merchant to the spender"]
            },
            "reasons": {
                "engine › daemon_rpc::live_node_tests::live_node_get_height_returns_a_plausible_value": "needs a live mainnet node",
                "monokulo › embed_domains::tests::live_system_resolver_reads_real_txt_records": "needs live DNS",
                "shared › haveno::tests::live_haveno_markets_api_prices_fiat_under_both_policies": "needs the live haveno.markets API",
                "shared › coinmarketcap::tests::live_coinmarketcap_api_quotes_usd": "needs the live CoinMarketCap API",
                "pos-stagenet-payments.spec.js › a 0-conf-trusted payment shows shared success and stays reviewable": "needs the live stagenet node and the funded test wallets"
            },
            "inconclusive": {
                "shared › coinmarketcap::tests::live_coinmarketcap_api_quotes_usd": "CoinMarketCap's keyless API still rate-limited this address (HTTP 429)"
            }
        }),
    );
}

struct Fixture {
    scratch: Scratch,
}

impl Fixture {
    fn new() -> Self {
        let scratch = Scratch::new("coverage-report");
        coverage_artifact(&scratch.join("coverage"));
        exploration_artifacts(&scratch.join("runs"));
        live_artifact(&scratch.join("live"));
        Fixture { scratch }
    }

    fn path(&self, rel: &str) -> String {
        self.scratch.join(rel).to_string_lossy().into_owned()
    }

    fn out(&self) -> PathBuf {
        self.scratch.join("site/testing")
    }

    fn build(&self, args: &[&str]) {
        let out = self.path("site/testing");
        let mut all = vec!["--out", out.as_str()];
        all.extend_from_slice(args);
        assert!(build(&all).unwrap().succeeded());
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
        "--live",
        &f.path("live"),
    ]);
    let pages = page_names(&f.out());
    assert_eq!(
        pages,
        [
            "coverage.html",
            "fuzzing.html",
            "index.html",
            "live.html",
            "properties.html",
            "scale.html",
            "screens.html",
            "tests.html"
        ]
    );
    // Each covered file links its annotated source.
    assert!(f
        .page("coverage.html")
        .contains("href=\"reports/rust/coverage/home/runner/crates/engine/src/scanner.rs.html\""));
    // A live failure's message is escaped.
    let live = f.page("live.html");
    assert!(
        live.contains("got: [&lt;script&gt;]") && !live.contains("<script>]"),
        "{live}"
    );
    // A pass that could not tell is counted as such, with why.
    assert!(
        live.contains("1 inconclusive")
            && live.contains("keyless API still rate-limited this address (HTTP 429)"),
        "{live}"
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
        &f.scratch.join("coverage/rust/junit.xml"),
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

/// Each input the report reads, as its sections show it, and each one
/// missing leaving its sections out while the front page still builds.
#[test]
fn each_input_builds_its_sections_and_a_missing_one_leaves_them_out() {
    struct Case {
        case: &'static str,
        /// What the fixture's artifacts differ by.
        change: fn(&Fixture),
        /// The build's inputs, each a flag and its folder in the fixture (or
        /// the build `--feature` names); `None` reads them without building.
        inputs: Option<&'static [(&'static str, &'static str)]>,
        check: fn(&Fixture),
    }
    let cases = [
        Case {
            case: "no inputs",
            change: |_| {},
            inputs: Some(&[]),
            check: |f| {
                assert_eq!(page_names(&f.out()), ["index.html"]);
                let page = f.page("index.html");
                assert!(page.contains("No test results have been published yet."));
                assert!(!page.contains("class=\"tabs\""));
                let badge: Value = read_json(&f.out().join("badge.json")).unwrap();
                assert_eq!(badge["message"], "unknown");
                let linked =
                    super::site_assets::assert_links_published(&page, &f.out().join("../assets"));
                assert!(linked.contains(&"styles.css") && linked.contains(&"manrope-500.woff2"));
            },
        },
        Case {
            case: "fuzzing alone",
            change: |_| {},
            inputs: Some(&[("--fuzz", "runs")]),
            check: |f| {
                assert_eq!(page_names(&f.out()), ["fuzzing.html", "index.html"]);
                assert!(f
                    .page("index.html")
                    .contains("This run published no test results."));
            },
        },
        Case {
            case: "coverage: the shipping figure and the stress run",
            change: |_| {},
            inputs: Some(&[("--coverage", "coverage")]),
            check: |f| {
                // Test tools stay out of the shipping figure and the badge.
                let (run, _) = inputs::coverage(&f.scratch.join("coverage")).unwrap();
                assert_eq!(
                    (run.shipping.lines.covered, run.shipping.lines.total),
                    (80, 90)
                );
                let wallet = run.crates.iter().find(|c| c.name == "cli-wallet").unwrap();
                assert!(wallet.tool);
                let badge: Value = read_json(&f.out().join("badge.json")).unwrap();
                assert_eq!(badge["message"], "88.9%");
                assert_eq!(badge["color"], "green");
                assert_eq!(badge["schemaVersion"], 1);
                assert!(!f.page("index.html").contains("not measured in this run"));
                // The stress run reads its named hardware file and fault settings.
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
                let page = f.page("scale.html");
                assert!(page.contains("Test CPU"));
                assert!(page.contains("every third request failed until block 2"));
                assert!(page.contains("<svg"));
            },
        },
        Case {
            case: "a suite without a manifest",
            change: |f| fs::remove_file(f.scratch.join("coverage/browser.json")).unwrap(),
            inputs: Some(&[("--coverage", "coverage")]),
            check: |f| assert!(f.page("index.html").contains("not measured in this run")),
        },
        Case {
            case: "only the named build of the nightly runs",
            change: |_| {},
            inputs: Some(&[
                ("--properties", "runs"),
                ("--fuzz", "runs"),
                ("--feature", "default"),
            ]),
            check: |f| {
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
                assert!(f
                    .page("properties.html")
                    .contains("Money matches the model default"));
            },
        },
        Case {
            case: "a live failure while its service was down",
            change: |f| {
                // Only the node test, which failed while its node was down.
                put(
                    &f.scratch.join("live/rust-junit.xml"),
                    junit_xml(&[(
                        "engine",
                        "daemon_rpc::live_node_tests::live_node_get_height_returns_a_plausible_value",
                        1.5,
                        r#"<failure message="connection refused"/>"#,
                    )]),
                );
            },
            inputs: Some(&[("--live", "live")]),
            check: |f| {
                let page = f.page("live.html");
                assert!(page.contains("All checks passed"), "{page}");
                assert!(page.contains("Service unreachable"), "{page}");
            },
        },
        Case {
            case: "a shot without a result",
            change: |f| {
                let manifest = f.scratch.join("coverage/screenshots/manifest.json");
                let mut shots: Value = read_json(&manifest).unwrap();
                shots[0]["status"] = "failed".into();
                shots[1].as_object_mut().unwrap().remove("status");
                put_json(&manifest, &shots);
            },
            inputs: None,
            check: |f| {
                // It keeps the earlier shot's failure.
                let out = f.out();
                fs::create_dir_all(&out).unwrap();
                let (gallery, _) = inputs::gallery(&f.scratch.join("coverage"), &out)
                    .unwrap()
                    .unwrap();
                assert_eq!(gallery.screens[0].status.as_deref(), Some("failed"));
            },
        },
    ];
    for Case {
        case,
        change,
        inputs,
        check,
    } in cases
    {
        let f = Fixture::new();
        change(&f);
        if let Some(inputs) = inputs {
            let mut args: Vec<String> = Vec::new();
            for (flag, value) in inputs {
                let value = if *flag == "--feature" {
                    value.to_string()
                } else {
                    f.path(value)
                };
                args.extend([flag.to_string(), value]);
            }
            f.build(&args.iter().map(String::as_str).collect::<Vec<_>>());
        }
        eprintln!("case: {case}");
        check(&f);
    }
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
    let out = f.path("site/testing");
    let error = build(&["--out", &out, "--coverage", &f.path("coverage")])
        .expect_err("a screenshot outside the artifact is refused");
    assert!(
        error.to_string().contains("outside the screenshots folder"),
        "{error}"
    );
}
