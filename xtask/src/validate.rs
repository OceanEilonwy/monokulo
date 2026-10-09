//! Validates the portable all-profile coverage artifact (`target/coverage`):
//! the manifests against docs/coverage-manifest.schema.json, every report
//! link, the screenshot gallery's files, the expected sources, and the
//! reviewed line floors in docs/coverage-line-baseline.json.

use crate::support::{files_under, html_links, read_json};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs, io,
    path::{Component, Path, PathBuf},
};

/// A failed check: the artifact is readable but not what it should be.
fn problem(text: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, text)
}

fn ensure(ok: bool, what: impl FnOnce() -> String) -> io::Result<()> {
    if ok {
        Ok(())
    } else {
        Err(problem(what()))
    }
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// The subset of JSON Schema the manifest schema uses: `$ref` into `$defs`,
/// `type`, `enum`, `minLength`, `minimum`, `required`, `properties`,
/// `additionalProperties`, `uniqueItems` and `items`.
fn check(schema: &Value, value: &Value, rule: &Value, at: &str) -> io::Result<()> {
    if let Some(reference) = rule["$ref"].as_str() {
        let name = reference.rsplit('/').next().unwrap_or("");
        return check(schema, value, &schema["$defs"][name], at);
    }
    if !rule["type"].is_null() {
        let kinds: Vec<&str> = match &rule["type"] {
            Value::Array(kinds) => kinds.iter().filter_map(Value::as_str).collect(),
            other => other.as_str().into_iter().collect(),
        };
        ensure(kinds.contains(&kind(value)), || {
            format!("{at}: expected {kinds:?}, got {}", kind(value))
        })?;
    }
    if let Some(allowed) = rule["enum"].as_array() {
        ensure(allowed.contains(value), || {
            format!("{at}: invalid value {value}")
        })?;
    }
    if let Value::String(s) = value {
        let min = rule["minLength"]
            .as_u64()
            .map_or(0, |n| usize::try_from(n).unwrap_or(usize::MAX));
        ensure(s.chars().count() >= min, || format!("{at}: empty string"))?;
    }
    if let (Some(n), Some(min)) = (value.as_i64(), rule["minimum"].as_i64()) {
        ensure(n >= min, || format!("{at}: below minimum"))?;
    }
    if let Value::Object(map) = value {
        for name in rule["required"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            ensure(map.contains_key(name), || format!("{at}: missing {name}"))?;
        }
        for (name, item) in map {
            let child = match rule["properties"].get(name) {
                Some(child) => child,
                None => rule
                    .get("additionalProperties")
                    .unwrap_or(&Value::Bool(true)),
            };
            ensure(child != &Value::Bool(false), || {
                format!("{at}: unexpected {name}")
            })?;
            if child.is_object() {
                check(schema, item, child, &format!("{at}.{name}"))?;
            }
        }
    }
    if let Value::Array(items) = value {
        if rule["uniqueItems"] == true {
            let distinct: BTreeSet<String> = items.iter().map(ToString::to_string).collect();
            ensure(distinct.len() == items.len(), || {
                format!("{at}: duplicates")
            })?;
        }
        if rule["items"].is_object() {
            for (i, item) in items.iter().enumerate() {
                check(schema, item, &rule["items"], &format!("{at}[{i}]"))?;
            }
        }
    }
    Ok(())
}

/// A file inside the artifact, named by a relative path that stays inside it.
fn local_file(root: &Path, relative: &str) -> io::Result<PathBuf> {
    let path = Path::new(relative);
    ensure(!relative.is_empty() && !path.is_absolute(), || {
        format!("invalid report path {relative}")
    })?;
    ensure(
        !path.components().any(|c| c == Component::ParentDir),
        || format!("path escapes artifact: {relative}"),
    )?;
    let full = root.join(path);
    ensure(full.is_file(), || format!("missing artifact: {relative}"))?;
    Ok(full)
}

/// Every local link and source in a report page names a file that exists.
fn check_html(path: &Path) -> io::Result<()> {
    let text = fs::read_to_string(path).map_err(|e| crate::support::at(path, e))?;
    let dir = path.parent().unwrap_or(Path::new(""));
    for target in html_links(&text) {
        ensure(dir.join(&target).is_file(), || {
            format!("broken link in {}: {target}", path.display())
        })?;
    }
    Ok(())
}

/// The schema and reviewed baseline the manifests are checked against.
struct Rules {
    schema: Value,
    baseline: Value,
}

/// One component's manifest: valid, passed, of the run's revision, its
/// report's links whole, and its lines at or above the reviewed floor
/// (trend-only when the tools changed, which is printed).
fn check_component(rules: &Rules, root: &Path, run: &Value, item: &Value) -> io::Result<()> {
    let name = item["component"]
        .as_str()
        .ok_or_else(|| problem("unnamed component in run.json".into()))?;
    ensure(item["status"] == "passed" && item["exit_code"] == 0, || {
        format!("{name}: tests failed")
    })?;
    local_file(root, item["log"].as_str().unwrap_or(""))?;
    let data: Value = read_json(&local_file(root, &format!("{name}.json"))?)?;
    check(&rules.schema, &data, &rules.schema, name)?;
    ensure(data["revision"] == run["revision"], || {
        format!("{name}: different source revision")
    })?;
    let unavailable = data["unavailable"]
        .as_array()
        .is_some_and(|u| !u.is_empty());
    ensure(data["test"]["status"] == "passed" && !unavailable, || {
        format!("{name}: incomplete metrics")
    })?;
    for metric in ["lines", "branches"] {
        let (covered, total) = (
            data[metric]["covered"].as_u64(),
            data[metric]["total"].as_u64(),
        );
        ensure(
            matches!((covered, total), (Some(c), Some(t)) if t > 0 && c <= t),
            || format!("{name}: invalid {metric}"),
        )?;
    }
    check_html(&local_file(root, data["report"].as_str().unwrap_or(""))?)?;
    let prior = &rules.baseline["components"][name];
    if data["tools"] == prior["tools"] {
        let floor = prior["floor_lines"].as_u64().unwrap_or(0);
        ensure(
            data["lines"]["covered"].as_u64().unwrap_or(0) >= floor,
            || format!("{name}: below reviewed line floor"),
        )?;
    } else {
        println!("{name}: tool versions changed; line floor is trend-only until reviewed");
    }
    Ok(())
}

/// Checks `root` (target/coverage) against the schema and baseline in `repo`.
/// Prints what changed tools leave unchecked; returns the first problem.
pub(crate) fn validate(repo: &Path, root: &Path) -> io::Result<()> {
    let rules = Rules {
        schema: read_json(&repo.join("docs/coverage-manifest.schema.json"))?,
        baseline: read_json(&repo.join("docs/coverage-line-baseline.json"))?,
    };
    check(
        &rules.schema,
        &read_json(&repo.join("docs/coverage-manifest.example.json"))?,
        &rules.schema,
        "fixture",
    )?;
    let run: Value = read_json(&local_file(root, "run.json")?)?;
    let components = run["components"].as_array().map_or(&[][..], Vec::as_slice);
    let names: Vec<&str> = components
        .iter()
        .filter_map(|c| c["component"].as_str())
        .collect();
    ensure(names == ["rust", "browser", "woocommerce"], || {
        "all run needs three ordered components".into()
    })?;
    for item in components {
        check_component(&rules, root, &run, item)?;
    }
    check_html(&local_file(root, "index.html")?)?;
    for area in ["rust", "browser", "woocommerce", "screenshots"] {
        for page in files_under(&root.join(area), |_| false)? {
            if page.extension().is_some_and(|e| e == "html") {
                check_html(&page)?;
            }
        }
    }
    check_screenshots(root)?;
    check_sources(root)
}

/// The gallery: enough screenshots, every expected group, each image a
/// distinct, present file.
fn check_screenshots(root: &Path) -> io::Result<()> {
    let entries: Value = read_json(&local_file(root, "screenshots/manifest.json")?)?;
    let entries = entries.as_array().map_or(&[][..], Vec::as_slice);
    ensure(entries.len() >= 10, || "fewer than ten screenshots".into())?;
    let groups: BTreeSet<&str> = entries.iter().filter_map(|e| e["group"].as_str()).collect();
    ensure(
        ["checkout", "pos", "challenge", "logs", "pos-timeline"]
            .iter()
            .all(|g| groups.contains(g)),
        || "missing screenshot group".into(),
    )?;
    let mut images = BTreeSet::new();
    for entry in entries {
        let image = entry["image"].as_str().unwrap_or("");
        ensure(image.starts_with("images/") && images.insert(image), || {
            format!("invalid or duplicate screenshot {image}")
        })?;
        let size =
            fs::metadata(local_file(root, &format!("screenshots/{image}"))?).map_or(0, |m| m.len());
        ensure(size > 0, || format!("empty screenshot {image}"))?;
    }
    Ok(())
}

/// The PHP and browser reports measured exactly the authored sources.
fn check_sources(root: &Path) -> io::Result<()> {
    let php: Value = read_json(&local_file(root, "woocommerce/summary.json")?)?;
    let php: BTreeSet<&str> = php["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| f["name"].as_str())
        .collect();
    ensure(
        php == BTreeSet::from(["monokulo.php", "class-wc-gateway-monokulo.php"]),
        || "unexpected PHP source".into(),
    )?;
    let browser: Value = read_json(&local_file(root, "browser/coverage-final.json")?)?;
    let browser: BTreeSet<String> = browser
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, _)| {
            Path::new(name)
                .file_name()
                .map_or(String::new(), |n| n.to_string_lossy().into_owned())
        })
        .collect();
    let expected: BTreeSet<String> = [
        "checkout.js",
        "challenge.js",
        "monokulo-client.js",
        "main.tsx",
        "timeline.ts",
        "engine-view.js",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    ensure(browser == expected, || "unexpected browser source".into())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::Scratch;
    use serde_json::json;

    #[test]
    fn the_checked_in_example_manifest_matches_the_schema() {
        let repo = crate::root();
        let schema: Value = read_json(&repo.join("docs/coverage-manifest.schema.json")).unwrap();
        let example: Value = read_json(&repo.join("docs/coverage-manifest.example.json")).unwrap();
        check(&schema, &example, &schema, "fixture").unwrap();
    }

    #[test]
    fn the_schema_checks_catch_what_they_are_for() {
        let schema = json!({"$defs": {"count": {"type": "integer", "minimum": 0}},
            "type": "object", "required": ["name", "lines"], "additionalProperties": false,
            "properties": {"name": {"type": "string", "minLength": 1, "enum": ["rust", "browser"]},
                "lines": {"$ref": "#/$defs/count"}, "tags": {"type": "array", "uniqueItems": true, "items": {"type": "string"}}}});
        let ok = |v: Value| check(&schema, &v, &schema, "m");
        assert!(ok(json!({"name": "rust", "lines": 3, "tags": ["a"]})).is_ok());
        for (bad, problem) in [
            (json!({"name": "rust"}), "missing lines"),
            (json!({"name": "php", "lines": 1}), "invalid value"),
            (json!({"name": "rust", "lines": -1}), "below minimum"),
            (json!({"name": "rust", "lines": 1.5}), "expected"),
            (
                json!({"name": "rust", "lines": 1, "extra": 1}),
                "unexpected extra",
            ),
            (
                json!({"name": "rust", "lines": 1, "tags": ["a", "a"]}),
                "duplicates",
            ),
            (json!({"name": "rust", "lines": 1, "tags": [1]}), "expected"),
        ] {
            let error = ok(bad.clone()).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(error.to_string().contains(problem), "{bad}: {error}");
        }
    }

    #[test]
    fn report_links_must_name_files_inside_the_artifact() {
        let scratch = Scratch::new("validate");
        let dir = scratch.path();
        fs::create_dir_all(dir.join("css")).unwrap();
        fs::write(dir.join("css/a b.css"), "").unwrap();
        let page = dir.join("index.html");
        fs::write(&page, r##"<link rel="stylesheet" href="css/a%20b.css"><a href="#top">x</a><a href="https://x">y</a><img src='data:image/png;base64,'><a href="index.html?x#y">z</a>"##).unwrap();
        assert!(check_html(&page).is_ok());
        fs::write(&page, r#"<!-- <a href="old.html"> --><script>const a = "<a href=\"x.html\">";</script><style>a{background:url(nope.png)}</style>"#).unwrap();
        assert!(
            check_html(&page).is_ok(),
            "script, style and comment bodies aren't links"
        );
        fs::write(&page, r#"<script src="missing.js"></script>"#).unwrap();
        assert!(check_html(&page)
            .unwrap_err()
            .to_string()
            .contains("missing.js"));
        assert!(local_file(dir, "../outside")
            .unwrap_err()
            .to_string()
            .contains("escapes"));
        assert!(local_file(dir, "/etc/passwd").is_err());
    }

    #[test]
    fn an_unnamed_component_is_a_problem_not_a_panic() {
        let scratch = Scratch::new("validate-run");
        let root = scratch.path();
        let repo = crate::root();
        fs::write(
            root.join("run.json"),
            r#"{"revision":"x","components":[{"status":"passed","exit_code":0},{"component":"rust"},{"component":"browser"},{"component":"woocommerce"}]}"#,
        )
        .unwrap();
        let error = validate(&repo, root).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("unnamed component"), "{error}");
    }
}
