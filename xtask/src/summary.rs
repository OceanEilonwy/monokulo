//! GitHub job summaries: `cargo xtask test-summary`, a table of `JUnit`
//! reports with their failures, and `cargo xtask coverage summary`, the
//! coverage components' table. Both print Markdown for `$GITHUB_STEP_SUMMARY`.

use crate::support::{escape_html, read_json};
use serde_json::Value;
use std::{fmt::Write, fs, io, path::Path};

pub(crate) const HELP: &str = "\
        test-summary TITLE LABEL=JUNIT...\n\
                      Print JUnit reports (nextest, Playwright, PHPUnit) as a GitHub job-summary table with\n\
                      their failures; a missing report is a row that says so";

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

/// One suite's counts and wall time.
struct Suite {
    counts: Counts,
    seconds: f64,
}

/// One suite's counts, wall time and failures. The run's wall time is the
/// root's own time where the tool records one (tests run in parallel, so
/// their times add up to more), else the sum.
fn read(
    label: &str,
    text: &str,
    failures: &mut Vec<Failure>,
    flaky: &mut Vec<Failure>,
) -> Result<Suite, String> {
    let doc = roxmltree::Document::parse(text).map_err(|e| e.to_string())?;
    let root = doc.root_element();
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
        } else if child("skipped").is_some() {
            counts.skipped += 1;
        } else if let Some(node) = child("flakyFailure").or_else(|| child("flakyError")) {
            // cargo-nextest's JUnit keeps a retried test's failed attempts.
            counts.flaky += 1;
            record(flaky, node);
        } else {
            counts.passed += 1;
        }
    }
    Ok(Suite { counts, seconds })
}

/// The Markdown for `cargo xtask test-summary TITLE LABEL=PATH ...`. A missing
/// file is a row that says so, since a suite that never reported is not a pass.
pub(crate) fn test_summary(title: &str, suites: &[&str]) -> String {
    let (mut failures, mut flaky) = (Vec::new(), Vec::new());
    // Each suite's counts and wall time, or why it has none.
    let mut rows: Vec<(String, Result<Suite, String>)> = Vec::new();
    for suite in suites {
        let (label, path) = suite.split_once('=').unwrap_or((suite, ""));
        let row = match fs::read_to_string(path) {
            Err(_) => Err("no report".to_string()),
            Ok(text) => read(label, &text, &mut failures, &mut flaky)
                .map_err(|e| format!("unreadable report: {e}")),
        };
        rows.push((label.to_string(), row));
    }
    let mut out = format!(
        "## {title}\n\n| Suite | Result | Passed | Failed | Flaky | Skipped | Time |\n| --- | --- | ---: | ---: | ---: | ---: | ---: |\n"
    );
    let mut total = Counts::default();
    for (label, row) in &rows {
        match row {
            Err(problem) => writeln!(out, "| {label} | ⚠️ {problem} | | | | | |").unwrap(),
            Ok(Suite { counts: c, seconds }) => {
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
        )
        .unwrap();
        assert_eq!((suite.counts.passed, suite.seconds), (2, 3.5));
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
