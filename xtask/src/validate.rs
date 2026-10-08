//! Validates the portable all-profile coverage artifact (`target/coverage`):
//! the manifests against docs/coverage-manifest.schema.json, every report
//! link, the screenshot gallery's files, the expected sources, and the
//! reviewed line floors in docs/coverage-line-baseline.json.

use regex::Regex;
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
    sync::LazyLock,
};

type Check = Result<(), String>;

fn ensure(ok: bool, problem: impl FnOnce() -> String) -> Check {
    if ok {
        Ok(())
    } else {
        Err(problem())
    }
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))
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
fn check(schema: &Value, value: &Value, rule: &Value, at: &str) -> Check {
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
        let min = rule["minLength"].as_u64().unwrap_or(0) as usize;
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
            let distinct: BTreeSet<String> = items.iter().map(|i| i.to_string()).collect();
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
fn local_file(root: &Path, relative: &str) -> Result<PathBuf, String> {
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

/// What a browser doesn't read as markup: comments, and the bodies of
/// scripts and styles (a script's own `src` still counts).
static NOT_MARKUP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)<!--.*?-->|(<script\b[^>]*>).*?(</script>)|(<style\b[^>]*>).*?(</style>)")
        .unwrap()
});

static LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<(a|img|script|link)\b[^>]*?\s(?:href|src)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'>]+))"#).unwrap()
});

fn unquote(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Every local link and source in a report page names a file that exists.
fn check_html(path: &Path) -> Check {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let text = NOT_MARKUP.replace_all(&text, "$1$2$3$4");
    for found in LINK.captures_iter(&text) {
        let reference = (2..=4)
            .find_map(|i| found.get(i))
            .map_or("", |m| m.as_str());
        let reference = reference.replace("&amp;", "&");
        if ["http:", "https:", "data:", "#", "mailto:", "javascript:"]
            .iter()
            .any(|p| reference.starts_with(p))
        {
            continue;
        }
        let target = unquote(
            reference
                .split('#')
                .next()
                .unwrap()
                .split('?')
                .next()
                .unwrap(),
        );
        if !target.is_empty() {
            ensure(path.parent().unwrap().join(&target).is_file(), || {
                format!("broken link in {}: {reference}", path.display())
            })?;
        }
    }
    Ok(())
}

fn html_pages(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            html_pages(&path, found);
        } else if path.extension().is_some_and(|e| e == "html") {
            found.push(path);
        }
    }
}

/// Checks `root` (target/coverage) against the schema and baseline in `repo`.
/// Prints what changed tools leave unchecked; returns the first problem.
pub(crate) fn validate(repo: &Path, root: &Path) -> Check {
    let schema = read_json(&repo.join("docs/coverage-manifest.schema.json"))?;
    let baseline = read_json(&repo.join("docs/coverage-line-baseline.json"))?;
    check(
        &schema,
        &read_json(&repo.join("docs/coverage-manifest.example.json"))?,
        &schema,
        "fixture",
    )?;
    let run = read_json(&local_file(root, "run.json")?)?;
    let components = run["components"].as_array().cloned().unwrap_or_default();
    let names: Vec<&str> = components
        .iter()
        .filter_map(|c| c["component"].as_str())
        .collect();
    ensure(names == ["rust", "browser", "woocommerce"], || {
        "all run needs three ordered components".into()
    })?;
    for item in &components {
        let name = item["component"].as_str().unwrap();
        ensure(item["status"] == "passed" && item["exit_code"] == 0, || {
            format!("{name}: tests failed")
        })?;
        local_file(root, item["log"].as_str().unwrap_or(""))?;
        let data = read_json(&local_file(root, &format!("{name}.json"))?)?;
        check(&schema, &data, &schema, name)?;
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
        let prior = &baseline["components"][name];
        if data["tools"] == prior["tools"] {
            let floor = prior["floor_lines"].as_u64().unwrap_or(0);
            ensure(
                data["lines"]["covered"].as_u64().unwrap_or(0) >= floor,
                || format!("{name}: below reviewed line floor"),
            )?;
        } else {
            println!("{name}: tool versions changed; line floor is trend-only until reviewed");
        }
    }
    check_html(&local_file(root, "index.html")?)?;
    for area in ["rust", "browser", "woocommerce", "screenshots"] {
        let mut pages = Vec::new();
        html_pages(&root.join(area), &mut pages);
        for page in pages {
            check_html(&page)?;
        }
    }
    let entries = read_json(&local_file(root, "screenshots/manifest.json")?)?;
    let entries = entries.as_array().cloned().unwrap_or_default();
    ensure(entries.len() >= 10, || "fewer than ten screenshots".into())?;
    let groups: BTreeSet<&str> = entries.iter().filter_map(|e| e["group"].as_str()).collect();
    ensure(
        ["checkout", "pos", "challenge", "logs", "pos-timeline"]
            .iter()
            .all(|g| groups.contains(g)),
        || "missing screenshot group".into(),
    )?;
    let mut images = BTreeSet::new();
    for entry in &entries {
        let image = entry["image"].as_str().unwrap_or("");
        ensure(image.starts_with("images/") && images.insert(image), || {
            format!("invalid or duplicate screenshot {image}")
        })?;
        let size =
            fs::metadata(local_file(root, &format!("screenshots/{image}"))?).map_or(0, |m| m.len());
        ensure(size > 0, || format!("empty screenshot {image}"))?;
    }
    let php = read_json(&local_file(root, "woocommerce/summary.json")?)?;
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
    let browser = read_json(&local_file(root, "browser/coverage-final.json")?)?;
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
    use serde_json::json;

    #[test]
    fn the_checked_in_example_manifest_matches_the_schema() {
        let repo = crate::root();
        let schema = read_json(&repo.join("docs/coverage-manifest.schema.json")).unwrap();
        let example = read_json(&repo.join("docs/coverage-manifest.example.json")).unwrap();
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
            assert!(error.contains(problem), "{bad}: {error}");
        }
    }

    #[test]
    fn report_links_must_name_files_inside_the_artifact() {
        let dir = std::env::temp_dir().join(format!("xtask-validate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
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
        assert!(check_html(&page).unwrap_err().contains("missing.js"));
        assert!(local_file(&dir, "../outside")
            .unwrap_err()
            .contains("escapes"));
        assert!(local_file(&dir, "/etc/passwd").is_err());
        let _ = fs::remove_dir_all(dir);
    }
}
