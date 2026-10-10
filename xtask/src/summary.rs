//! GitHub job summaries: `cargo xtask test-summary`, a table of `JUnit`
//! reports with their failures, and `cargo xtask coverage summary`, the
//! coverage components' table. Both print Markdown for `$GITHUB_STEP_SUMMARY`.

use crate::support::{escape_html, files_under, read_json, root};
use serde_json::Value;
use std::{fmt::Write, fs, io, path::Path};
use syn::visit::{self, Visit};

pub(crate) const HELP: &str = "\
        test-summary TITLE LABEL=JUNIT...\n\
                      Print JUnit reports (nextest, Playwright, PHPUnit) as a GitHub job-summary table with\n\
                      their failures and skipped tests; a missing report is a row that says so";

const MESSAGE_LIMIT: usize = 3000;

#[derive(Default, Clone, Copy)]
struct Counts {
    passed: u64,
    failed: u64,
    flaky: u64,
    skipped: u64,
}

struct Failure {
    label: String,
    name: String,
    text: String,
}

/// A test that did not run, and why when the report or source says.
struct Skip {
    label: String,
    name: String,
    reason: Option<String>,
}

/// Why a skipped case was skipped. A `<skipped message>` (or body) where the
/// tool writes one; Playwright leaves `<skipped/>` empty and writes the
/// reason given to `test.skip` or `test.fixme` as that annotation's property.
pub(crate) fn skip_reason(case: roxmltree::Node, skipped: roxmltree::Node) -> Option<String> {
    let property = case
        .descendants()
        .filter(|n| n.has_tag_name("property"))
        .find(|n| matches!(n.attribute("name"), Some("skip" | "fixme")))
        .and_then(|n| n.attribute("value"));
    [skipped.attribute("message"), skipped.text(), property]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|reason| !reason.is_empty())
        .map(str::to_string)
}

/// Every `#[ignore]`d test in the Rust files under `root`, named by its file
/// and module path, with its reason. nextest's `JUnit` report leaves ignored
/// tests out altogether, and neither its test list nor libtest's says why a
/// test is ignored, so the reason is read where it is written. Reading the
/// source needs no build, so it works the same after a plain or a coverage
/// run on every platform. A file that does not parse is not one a test
/// binary was built from. `cargo xtask live` reads the same list to check
/// each one is run daily or says why not.
pub(crate) fn ignored_tests(root: &Path) -> io::Result<Vec<(String, Option<String>)>> {
    struct Tests {
        modules: Vec<String>,
        file: String,
        found: Vec<(String, Option<String>)>,
    }
    impl Visit<'_> for Tests {
        fn visit_item_mod(&mut self, item: &syn::ItemMod) {
            self.modules.push(item.ident.to_string());
            visit::visit_item_mod(self, item);
            self.modules.pop();
        }
        fn visit_item_fn(&mut self, item: &syn::ItemFn) {
            for attr in item.attrs.iter().filter(|a| a.path().is_ident("ignore")) {
                let reason = match &attr.meta {
                    syn::Meta::NameValue(syn::MetaNameValue {
                        value:
                            syn::Expr::Lit(syn::ExprLit {
                                lit: syn::Lit::Str(reason),
                                ..
                            }),
                        ..
                    }) => Some(reason.value()),
                    _ => None,
                };
                let path = self
                    .modules
                    .iter()
                    .cloned()
                    .chain([item.sig.ident.to_string()])
                    .collect::<Vec<_>>()
                    .join("::");
                self.found.push((format!("{} › {path}", self.file), reason));
            }
            visit::visit_item_fn(self, item);
        }
    }
    let prune = |dir: &Path| {
        dir.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
            n.starts_with('.') || ["target", "node_modules", "vendor"].contains(&n)
        })
    };
    let mut tests = Tests {
        modules: Vec::new(),
        file: String::new(),
        found: Vec::new(),
    };
    for path in files_under(root, prune)? {
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let Some(file) = fs::read_to_string(&path)
            .ok()
            .and_then(|text| syn::parse_file(&text).ok())
        else {
            continue;
        };
        let relative = path.strip_prefix(root).unwrap_or(&path);
        tests.file = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        tests.visit_file(&file);
    }
    Ok(tests.found)
}

/// A failed or flaky case's message: its `message` attribute, then its body
/// when the body adds something, cut to a readable length.
fn message(node: roxmltree::Node) -> String {
    let mut text = node.attribute("message").unwrap_or("").trim().to_string();
    let body = node.text().unwrap_or("").trim();
    if !body.is_empty() && !text.contains(body) {
        text = if text.is_empty() {
            body.to_string()
        } else {
            format!("{text}\n{body}")
        };
    }
    if text.chars().count() > MESSAGE_LIMIT {
        text = text.chars().take(MESSAGE_LIMIT).collect::<String>() + "\n…";
    }
    text
}

/// One suite's counts and wall time, whether cargo-nextest wrote it, and
/// the name of every case it ran.
struct Suite {
    counts: Counts,
    seconds: f64,
    nextest: bool,
    names: Vec<String>,
}

/// One suite's counts, wall time, failures and skipped tests. The run's wall
/// time is the root's own time where the tool records one (tests run in
/// parallel, so their times add up to more), else the sum.
fn read(
    label: &str,
    text: &str,
    failures: &mut Vec<Failure>,
    flaky: &mut Vec<Failure>,
    skips: &mut Vec<Skip>,
) -> Result<Suite, String> {
    let doc = roxmltree::Document::parse(text).map_err(|e| e.to_string())?;
    let root = doc.root_element();
    let nextest = root.attribute("name") == Some("nextest-run");
    let cases: Vec<_> = root
        .descendants()
        .filter(|n| n.has_tag_name("testcase"))
        .collect();
    let mut seconds = root.attribute("time").map(str::to_string);
    if seconds.is_none() && root.has_tag_name("testsuites") {
        let suites: Vec<_> = root
            .children()
            .filter(|n| n.has_tag_name("testsuite"))
            .collect();
        if suites.len() == 1 {
            seconds = suites[0].attribute("time").map(str::to_string);
        }
    }
    let seconds = match seconds.and_then(|s| s.parse().ok()) {
        Some(s) => s,
        None => cases
            .iter()
            .map(|c| {
                c.attribute("time")
                    .and_then(|t| t.parse::<f64>().ok())
                    .unwrap_or(0.0)
            })
            .sum(),
    };
    let mut counts = Counts::default();
    let names = cases
        .iter()
        .filter_map(|case| case.attribute("name"))
        .map(str::to_string)
        .collect();
    for case in cases {
        let child = |tag: &str| case.children().find(|c| c.has_tag_name(tag));
        let name = [case.attribute("classname"), case.attribute("name")]
            .into_iter()
            .flatten()
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join(" › ");
        let record = |list: &mut Vec<Failure>, node| {
            list.push(Failure {
                label: label.to_string(),
                name: name.clone(),
                text: message(node),
            });
        };
        if let Some(node) = child("failure").or_else(|| child("error")) {
            counts.failed += 1;
            record(failures, node);
        } else if let Some(node) = child("skipped") {
            counts.skipped += 1;
            skips.push(Skip {
                label: label.to_string(),
                name: name.clone(),
                reason: skip_reason(case, node),
            });
        } else if let Some(node) = child("flakyFailure").or_else(|| child("flakyError")) {
            // cargo-nextest's JUnit keeps a retried test's failed attempts.
            counts.flaky += 1;
            record(flaky, node);
        } else {
            counts.passed += 1;
        }
    }
    Ok(Suite {
        counts,
        seconds,
        nextest,
        names,
    })
}

/// Each suite's skipped tests under a heading of its own. Tests that share a
/// reason are one collapsed line saying how many and why, so a suite that
/// skips a dozen tests for one reason stays a line long.
fn write_skips(out: &mut String, skips: &[Skip]) {
    let mut labels: Vec<&str> = Vec::new();
    for skip in skips {
        if !labels.contains(&skip.label.as_str()) {
            labels.push(&skip.label);
        }
    }
    for label in labels {
        let mut groups: Vec<(Option<&str>, Vec<&str>)> = Vec::new();
        for skip in skips.iter().filter(|s| s.label == label) {
            let reason = skip.reason.as_deref();
            match groups.iter_mut().find(|(r, _)| *r == reason) {
                Some((_, names)) => names.push(&skip.name),
                None => groups.push((reason, vec![&skip.name])),
            }
        }
        // The biggest groups first; a stable sort keeps the rest in report order.
        groups.sort_by_key(|(_, names)| std::cmp::Reverse(names.len()));
        let total: usize = groups.iter().map(|(_, names)| names.len()).sum();
        writeln!(out, "### Skipped: {label} ({total})\n").unwrap();
        let reason = |r: Option<&str>| escape_html(r.unwrap_or("no reason given"));
        let mut single = Vec::new();
        for (r, names) in &groups {
            if let [name] = names[..] {
                single.push(format!(
                    "- <code>{}</code>: {}",
                    escape_html(name),
                    reason(*r)
                ));
                continue;
            }
            writeln!(
                out,
                "<details><summary>{} tests: {}</summary>\n",
                names.len(),
                reason(*r)
            )
            .unwrap();
            for name in names {
                writeln!(out, "- <code>{}</code>", escape_html(name)).unwrap();
            }
            out.push_str("</details>\n\n");
        }
        for line in single {
            writeln!(out, "{line}").unwrap();
        }
        out.push('\n');
    }
}

/// The Markdown for `cargo xtask test-summary TITLE LABEL=PATH ...`. A missing
/// file is a row that says so, since a suite that never reported is not a pass.
pub(crate) fn test_summary(title: &str, suites: &[&str]) -> String {
    summarize(&root(), title, suites)
}

/// [`test_summary`], with a nextest report's ignored tests read from the Rust
/// source under `source`.
fn summarize(source: &Path, title: &str, suites: &[&str]) -> String {
    let (mut failures, mut flaky, mut skips) = (Vec::new(), Vec::new(), Vec::new());
    // Each suite's counts and wall time, or why it has none.
    let mut rows: Vec<(String, Result<Suite, String>)> = Vec::new();
    for suite in suites {
        let (label, path) = suite.split_once('=').unwrap_or((suite, ""));
        let mut row = match fs::read_to_string(path) {
            Err(_) => Err("no report".to_string()),
            Ok(text) => read(label, &text, &mut failures, &mut flaky, &mut skips)
                .map_err(|e| format!("unreadable report: {e}")),
        };
        if let Ok(suite) = &mut row {
            if suite.nextest {
                match ignored_tests(source) {
                    Ok(mut ignored) => {
                        // A run with --run-ignored (the daily live run) ran
                        // some: the report names each by its module path
                        // in its binary, which ends with its path in its file.
                        ignored.retain(|(name, _)| {
                            let path = name.split_once(" › ").map_or(name.as_str(), |(_, p)| p);
                            !suite
                                .names
                                .iter()
                                .any(|ran| ran == path || ran.ends_with(&format!("::{path}")))
                        });
                        suite.counts.skipped += ignored.len() as u64;
                        skips.extend(ignored.into_iter().map(|(name, reason)| Skip {
                            label: label.to_string(),
                            name,
                            reason,
                        }));
                    }
                    Err(e) => row = Err(format!("unreadable source: {e}")),
                }
            }
        }
        rows.push((label.to_string(), row));
    }
    let mut out = format!(
        "## {title}\n\n| Suite | Result | Passed | Failed | Flaky | Skipped | Time |\n| --- | --- | ---: | ---: | ---: | ---: | ---: |\n"
    );
    let mut total = Counts::default();
    for (label, row) in &rows {
        match row {
            Err(problem) => writeln!(out, "| {label} | ⚠️ {problem} | | | | | |").unwrap(),
            Ok(Suite {
                counts: c, seconds, ..
            }) => {
                total.passed += c.passed;
                total.failed += c.failed;
                total.flaky += c.flaky;
                total.skipped += c.skipped;
                let result = if c.failed > 0 {
                    "❌ failed"
                } else if c.flaky > 0 {
                    "⚠️ passed on retry"
                } else {
                    "✅ passed"
                };
                writeln!(
                    out,
                    "| {label} | {result} | {} | {} | {} | {} | {seconds:.0}s |",
                    c.passed, c.failed, c.flaky, c.skipped
                )
                .unwrap();
            }
        }
    }
    writeln!(
        out,
        "| **Total** | | **{}** | **{}** | **{}** | **{}** | |\n",
        total.passed, total.failed, total.flaky, total.skipped
    )
    .unwrap();
    for (heading, listed) in [
        ("Failures", &failures),
        ("Flaky: failed, then passed on retry", &flaky),
    ] {
        if listed.is_empty() {
            continue;
        }
        writeln!(out, "### {heading} ({})\n", listed.len()).unwrap();
        for f in listed {
            let first = f.text.lines().next().unwrap_or("no message");
            let first: String = first.chars().take(200).collect();
            writeln!(
                out,
                "<details><summary><b>{}</b>: <code>{}</code>: {}</summary>\n\n```\n{}\n```\n</details>\n",
                f.label,
                escape_html(&f.name),
                escape_html(&first),
                f.text.replace("```", "`\u{200b}``")
            )
            .unwrap();
        }
    }
    write_skips(&mut out, &skips);
    out
}

/// The Markdown for `cargo xtask coverage summary`: each component's tests,
/// line and branch counts, and report, from the manifests in `root`.
pub(crate) fn coverage_summary(root: &Path) -> io::Result<String> {
    let mut out = String::from(
        "## Coverage\n\n| Component | Tests | Lines | Branches | Report |\n| --- | --- | ---: | ---: | --- |\n",
    );
    let run = root.join("run.json");
    if !run.is_file() {
        out.push_str("| all | unavailable | unavailable | unavailable | [artifact](.) |\n");
        return Ok(out);
    }
    let run: Value = read_json(&run)?;
    for item in run["components"].as_array().into_iter().flatten() {
        let name = item["component"].as_str().unwrap_or("?");
        let manifest = root.join(format!("{name}.json"));
        let data: Value = if manifest.is_file() {
            read_json(&manifest)?
        } else {
            Value::Null
        };
        let metric = |key: &str| match (data[key]["covered"].as_u64(), data[key]["total"].as_u64())
        {
            (Some(c), Some(t)) => format!("{c}/{t}"),
            _ => "unavailable".into(),
        };
        let link = match data["report"].as_str() {
            Some(report) if root.join(report).is_file() => format!("`{report}`"),
            _ => "unavailable".into(),
        };
        writeln!(
            out,
            "| {name} | {} | {} | {} | {link} |",
            item["status"].as_str().unwrap_or("?"),
            metric("lines"),
            metric("branches")
        )
        .unwrap();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::Scratch;

    #[test]
    fn a_suite_counts_every_outcome_and_lists_failures_and_retries() {
        let scratch = Scratch::new("summary-outcomes");
        let report = scratch.join("junit.xml");
        fs::write(
            &report,
            r#"<testsuites time="12.4"><testsuite><testcase classname="engine" name="pays"/>
            <testcase classname="engine" name="breaks"><failure message="assertion failed: 1 == 2">left: 1 &lt; right</failure></testcase>
            <testcase name="later"><skipped/></testcase>
            <testcase classname="engine" name="wobbles"><flakyFailure message="timed out"/></testcase></testsuite></testsuites>"#,
        )
        .unwrap();
        let summary = test_summary(
            "Tests",
            &[&format!("rust={}", report.display()), "php=missing.xml"],
        );
        assert!(
            summary.contains("| rust | ❌ failed | 1 | 1 | 1 | 1 | 12s |"),
            "{summary}"
        );
        assert!(summary.contains("| php | ⚠️ no report | | | | | |"));
        assert!(summary.contains("### Failures (1)"));
        assert!(summary.contains("<code>engine › breaks</code>: assertion failed: 1 == 2"));
        assert!(summary.contains("assertion failed: 1 == 2\nleft: 1 < right"));
        assert!(summary.contains("### Flaky: failed, then passed on retry (1)"));
    }

    #[test]
    fn a_report_without_a_root_time_adds_its_cases_up() {
        let mut failures = Vec::new();
        let suite = read(
            "x",
            r#"<testsuite><testcase name="a" time="1.5"/><testcase name="b" time="2"/></testsuite>"#,
            &mut failures,
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!((suite.counts.passed, suite.seconds), (2, 3.5));
    }

    #[test]
    fn skipped_cases_are_listed_by_reason_with_shared_reasons_collapsed() {
        let scratch = Scratch::new("summary-skips");
        let report = scratch.join("junit.xml");
        // As Playwright's JUnit reporter writes them: `<skipped/>` is empty,
        // and the reason given to test.skip or test.fixme is the property.
        let webkit = "WebKit can't launch here (missing libs)";
        fs::write(
            &report,
            format!(
                r#"<testsuites><testsuite name="pos.spec.js">
            <testcase classname="pos.spec.js" name="fits a phone on WebKit"><properties><property name="skip" value="{webkit}"/></properties><skipped/></testcase>
            <testcase classname="pos.spec.js" name="fits a tablet on WebKit"><properties><property name="skip" value="{webkit}"/></properties><skipped/></testcase>
            <testcase classname="pos.spec.js" name="scans &lt;a&gt; code"><properties><property name="fixme" value="camera stub &amp; lens"/></properties><skipped/></testcase>
            <testcase classname="pos.spec.js" name="says why itself"><skipped message="no wallet configured"/></testcase>
            <testcase classname="pos.spec.js" name="bare"><skipped/></testcase>
            <testcase classname="pos.spec.js" name="runs"/></testsuite></testsuites>"#
            ),
        )
        .unwrap();
        let summary = test_summary("Browser", &[&format!("browser={}", report.display())]);
        assert!(
            summary.contains("| browser | ✅ passed | 1 | 0 | 0 | 5 |"),
            "{summary}"
        );
        assert!(summary.contains(
            "### Skipped: browser (5)\n\n<details><summary>2 tests: WebKit can&#39;t launch here (missing libs)</summary>\n\n\
             - <code>pos.spec.js › fits a phone on WebKit</code>\n\
             - <code>pos.spec.js › fits a tablet on WebKit</code>\n</details>\n\n\
             - <code>pos.spec.js › scans &lt;a&gt; code</code>: camera stub &amp; lens\n\
             - <code>pos.spec.js › says why itself</code>: no wallet configured\n\
             - <code>pos.spec.js › bare</code>: no reason given\n"
        ));
        // A run that skipped nothing has no section.
        assert!(!test_summary("Browser", &["browser=missing.xml"]).contains("Skipped:"));
    }

    #[test]
    fn a_nextest_report_lists_the_ignored_tests_its_junit_leaves_out() {
        let scratch = Scratch::new("summary-ignored");
        let source = scratch.join("src");
        fs::create_dir_all(source.join("crates/net/tests")).unwrap();
        fs::write(
            source.join("crates/net/src.rs"),
            r#"#[test] fn runs() {}
            mod live { mod dns {
                #[tokio::test]
                #[ignore = "needs live DNS: \
                            resolves real records"]
                async fn resolves() {}
            } }
            #[test] #[ignore] fn bare() {}"#,
        )
        .unwrap();
        // Neither a file that does not parse nor one in a build directory is
        // a test the run built.
        fs::write(source.join("crates/net/tests/broken.rs"), "#[ignore] fn (").unwrap();
        fs::create_dir_all(source.join("target/debug")).unwrap();
        fs::write(
            source.join("target/debug/copy.rs"),
            "#[ignore] fn copy() {}",
        )
        .unwrap();
        let report = scratch.join("junit.xml");
        fs::write(
            &report,
            r#"<testsuites name="nextest-run" time="1"><testsuite name="net"><testcase classname="net" name="runs"/></testsuite></testsuites>"#,
        )
        .unwrap();
        let summary = summarize(&source, "Rust", &[&format!("rust={}", report.display())]);
        assert!(
            summary.contains("| rust | ✅ passed | 1 | 0 | 0 | 2 | 1s |"),
            "{summary}"
        );
        assert!(summary.contains(
            "- <code>crates/net/src.rs › live::dns::resolves</code>: needs live DNS: resolves real records\n\
             - <code>crates/net/src.rs › bare</code>: no reason given\n"
        ));
        // An ignored test the run ran anyway (--run-ignored) is not skipped.
        fs::write(
            &report,
            r#"<testsuites name="nextest-run" time="1"><testsuite name="net"><testcase classname="net" name="net::live::dns::resolves"/></testsuite></testsuites>"#,
        )
        .unwrap();
        let summary = summarize(&source, "Rust", &[&format!("rust={}", report.display())]);
        assert!(
            summary.contains("| rust | ✅ passed | 1 | 0 | 0 | 1 | 1s |"),
            "{summary}"
        );
        assert!(!summary.contains("resolves</code>"), "{summary}");
        // Only nextest leaves its ignored tests out; another tool's report
        // already says what it skipped.
        fs::write(
            &report,
            r#"<testsuites><testsuite><testcase name="runs"/></testsuite></testsuites>"#,
        )
        .unwrap();
        let summary = summarize(&source, "Rust", &[&format!("rust={}", report.display())]);
        assert!(!summary.contains("Skipped:"), "{summary}");
    }

    #[test]
    fn every_ignored_test_in_the_workspace_says_why() {
        let bare: Vec<_> = ignored_tests(&root())
            .unwrap()
            .into_iter()
            .filter(|(_, reason)| reason.as_deref().is_none_or(|r| r.trim().is_empty()))
            .map(|(name, _)| name)
            .collect();
        assert!(
            bare.is_empty(),
            "give each #[ignore] a reason (#[ignore = \"needs ...\"]), for the job summary: {bare:?}"
        );
    }

    #[test]
    fn coverage_summary_says_what_is_unavailable() {
        let scratch = Scratch::new("summary-coverage");
        let dir = scratch.path();
        assert!(coverage_summary(dir)
            .unwrap()
            .contains("| all | unavailable |"));
        fs::write(dir.join("run.json"), r#"{"components":[{"component":"rust","status":"passed"},{"component":"browser","status":"failed"}]}"#).unwrap();
        fs::write(dir.join("rust.json"), r#"{"lines":{"covered":9,"total":10},"branches":{"covered":1,"total":2},"report":"rust/index.html"}"#).unwrap();
        fs::create_dir_all(dir.join("rust")).unwrap();
        fs::write(dir.join("rust/index.html"), "").unwrap();
        let summary = coverage_summary(dir).unwrap();
        assert!(
            summary.contains("| rust | passed | 9/10 | 1/2 | `rust/index.html` |"),
            "{summary}"
        );
        assert!(summary.contains("| browser | failed | unavailable | unavailable | unavailable |"));
    }
}
