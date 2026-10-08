#!/usr/bin/env python3
"""Builds the quality report (GitHub Pages: /quality/) from CI artifacts.

Every input is optional; the page says what it has no data for.

    scripts/quality-site.py --out site/quality \
        --coverage DIR    the joined coverage artifact (coverage-<sha>), or
                          target/coverage after `cargo xtask coverage all`
        --properties DIR  an engine-properties artifact
        --fuzz DIR        a folder holding engine-fuzz artifacts (one per target)
        --scale DIR       an engine-scale-measurements artifact
        --sources FILE    JSON naming the run behind each input:
                          {"coverage": {"run_id": 1, "sha": "...", "date": "..."}, ...}

It writes the page (web/quality/index.html), its data (data.json), the
screenshot gallery (gallery/), the annotated coverage reports (reports/)
and a shields.io endpoint for the coverage badge (badge.json). Only the
build named by --feature (zmq unless told otherwise) is shown for the
property and fuzz runs, which run each build separately.
"""

import argparse
import glob
import html
import json
import re
import shutil
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TEMPLATE = ROOT / "web/quality/index.html"
THEME = ROOT / "crates/monokulo/src/views/theme.css"
FONTS = [ROOT / f"crates/monokulo/static/manrope-{w}.woff2" for w in (500, 700, 800)]
# Crates that only exist to test the others: shown, but kept out of the
# shipping figure and the badge.
TEST_TOOLS = {"cli-wallet", "e2e-harness", "engine-test-support", "mock-woocommerce"}
THUMB_WIDTH = {"mobile-portrait": 280, "small-portrait": 280, "element": 360}
THUMB_DEFAULT_WIDTH = 480


def load(path):
    return json.loads(Path(path).read_text())


def cov(entry):
    return [entry["lines"]["covered"], entry["lines"]["total"], entry["branches"]["covered"], entry["branches"]["total"]]


def junit(path):
    """[class, name, seconds, passed|failed|skipped] for every test case."""
    rows = []
    for case in ET.parse(path).getroot().iter("testcase"):
        if case.find("failure") is not None or case.find("error") is not None:
            status = "failed"
        elif case.find("skipped") is not None:
            status = "skipped"
        else:
            status = "passed"
        rows.append([case.get("classname") or "", case.get("name"), round(float(case.get("time") or 0), 3), status])
    return rows


def coverage(src, out):
    """Coverage totals, per-crate and per-file Rust figures, and every test result."""
    data = {"components": {c["component"]: c["status"] for c in load(src / "run.json")["components"]}}
    data["revision"] = load(src / "run.json").get("revision")
    data["totals"] = {}
    data["tools"] = {}
    for name in ("rust", "browser", "woocommerce"):
        manifest = src / f"{name}.json"
        if manifest.is_file():
            m = load(manifest)
            data["totals"][name] = cov(m)
            data["tools"][name] = m.get("tools", {}).get("collector")
    crates = []
    if (src / "rust-crates.json").is_file():
        for c in load(src / "rust-crates.json"):
            page = src / c["report"]
            files = []
            if page.is_file():
                rows = re.findall(r'<tr><td><a href="([^"]+)">([^<]+)</a></td><td>(\d+)/(\d+)</td><td>(\d+)/(\d+)</td></tr>', page.read_text())
                for href, name, l1, l2, b1, b2 in rows:
                    # The crate pages link the annotated source relative to themselves.
                    link = (Path("reports") / Path(c["report"]).parent / href).as_posix()
                    link = re.sub(r"[^/]+/\.\./", "", link)
                    files.append([html.unescape(name), int(l1), int(l2), int(b1), int(b2), link])
            crates.append({"name": c["component"], "cov": cov(c), "tool": c["component"] in TEST_TOOLS,
                           "files": files, "unmeasured": c.get("unavailable_files", [])})
    data["crates"] = crates
    tests = {"rust": [], "browser": [], "woocommerce": []}
    if (src / "rust/junit.xml").is_file():
        tests["rust"] = junit(src / "rust/junit.xml")
    for file, kind in (("junit-fixture.xml", "fixture"), ("junit-real-binaries.xml", "real binaries")):
        if (src / "browser" / file).is_file():
            tests["browser"] += [row + [kind] for row in junit(src / "browser" / file)]
    if (src / "woocommerce/junit.xml").is_file():
        tests["woocommerce"] = junit(src / "woocommerce/junit.xml")
    data["tests"] = tests
    reports = {}
    for name in ("rust", "browser", "woocommerce", "stress"):
        if (src / name).is_dir():
            shutil.copytree(src / name, out / "reports" / name, dirs_exist_ok=True)
            if (src / name / "index.html").is_file():
                reports[name] = f"reports/{name}/index.html"
    data["reports"] = reports
    return data


def ship_totals(crates):
    shipped = [c["cov"] for c in crates if not c["tool"]]
    return [sum(c[i] for c in shipped) for i in range(4)]


def stress(run_file):
    """The one-CPU stress run (CI) or the weekly scale run, as the page draws them."""
    run = load(run_file)
    scenario = run.get("scenario", {})

    def point(result, name):
        fx = result.get("fixture", {})
        ticks = fx.get("points", [])
        peak = result.get("peak_resident_bytes") or max(
            [(t.get("process_memory") or {}).get("peak_resident_bytes") or 0 for t in ticks] or [0])
        return {
            "name": name, "status": result.get("status"), "detail": result.get("detail"),
            "tenants": result.get("tenants") or fx.get("tenants"),
            "ticks": [[t["phase"], t["duration_ms"], t["lagging_tenants"], t["oldest_lag_blocks"]] for t in ticks],
            "peak_mb": round(peak / 1048576, 1) if peak else None,
            "http_max_us": fx.get("http_max_latency_us"), "db_write_max_us": fx.get("db_write_query_max_us"),
            "rpc_calls": fx.get("rpc_calls"), "rpc_failures": fx.get("rpc_failures"), "rpc_delay_ms": fx.get("rpc_delay_ms"),
            "rpc_fail_every": fx.get("rpc_fail_every"), "rpc_fail_until_height": fx.get("rpc_fail_until_height"),
            "custody_slots": fx.get("custody_slots"), "custody_delay_ms": fx.get("custody_delay_ms"),
            "custody_scans": fx.get("custody_scans_completed"), "custody_max_wait_us": fx.get("custody_max_wait_us"),
            "lock_ms": fx.get("write_lock_hold_ms_per_tick"), "measured_ticks": fx.get("measured_ticks"), "drain_ticks": fx.get("drain_ticks"),
        }

    hardware = run.get("hardware", {})
    if not isinstance(hardware, dict):
        # The run names the file it wrote the hardware to.
        beside = Path(run_file).parent / "hardware.json"
        hardware = load(beside) if beside.is_file() else {}
    return {
        "profile": run.get("profile"), "started": run.get("started_at_utc"),
        "budget_ms": scenario.get("poll_interval_ms"), "max_lag_blocks": scenario.get("max_oldest_lag_blocks"),
        "max_http_ms": scenario.get("max_http_latency_ms"), "fault_lock_ms": scenario.get("fault_sqlite_lock_ms"),
        "points": [point(r, f"point-{r.get('tenants')}") for r in run.get("results", [])],
        "faults": [point(f, f.get("file")) for f in run.get("faults", [])],
        "hardware": {k: hardware.get(k) for k in ("cpu_model", "effective_cores", "sqlite_version", "selected_cpu")},
    }


def properties(src, feature):
    """The nightly property run of one build: its settings, scenario counts and every property."""
    reports = [p for p in glob.glob(str(src / "**/engine-exploration/properties/report.json"), recursive=True)
               if load(p).get("settings", {}).get("ENGINE_FEATURES", "") == ("" if feature == "default" else feature)]
    if not reports:
        return None
    report = load(reports[0])
    junits = glob.glob(str(src / "**/nextest/ci/junit.xml"), recursive=True)
    return {
        "revision": report.get("revision"), "settings": report.get("settings", {}),
        "cases": report.get("semantic_cases"), "observations": report.get("semantic_observations", {}),
        "tests": junit(junits[0]) if junits else [],
    }


def fuzz(src, feature):
    """Last night's campaign for each fuzz target of one build."""
    targets = {}
    for path in sorted(glob.glob(str(src / "**/engine-exploration/fuzz/*/*/*/*/report.json"), recursive=True)):
        parts = Path(path).parts
        i = len(parts) - 6
        target, build = parts[i + 1], parts[i + 2]
        if build != feature:
            continue
        r = load(path)
        ex = r.get("exploration") or {}
        final = ex.get("final") or {}
        targets[target] = {
            "target": target, "status": r.get("status"), "seconds": round(r.get("wall_seconds") or 0),
            "corpus": (r.get("corpus") or {}).get("files"), "new_inputs": r.get("new_unique_inputs"),
            "edges": final.get("coverage"), "edge_growth": ex.get("coverage_growth"), "execs": final.get("executions"),
            "cases": r.get("semantic_cases"), "observations": r.get("semantic_observations") or {},
        }
    return sorted(targets.values(), key=lambda t: -(t["edges"] or 0))


def gallery(src, out):
    """Every screenshot as a thumbnail for the grid and the full image for the viewer."""
    manifest = src / "screenshots/manifest.json"
    if not manifest.is_file():
        return None
    try:
        from PIL import Image
    except ImportError:
        sys.exit("the screenshot gallery needs Pillow (apt install python3-pil, or pip install pillow)")
    shots = load(manifest)
    stages = {}
    for shot in shots:
        if shot.get("retry"):
            continue
        key = (shot["group"], shot["stage"])
        stage = stages.setdefault(key, {"group": shot["group"], "stage": shot["stage"], "test": shot["test"],
                                        "status": shot.get("status"), "report": shot.get("report"), "images": {}, "count": 0})
        stage["count"] += 1
        if shot.get("status") != "passed":
            stage["status"] = shot.get("status")
        if shot["theme"] in stage["images"].get(shot["shape"], {}):
            continue
        name = f"{shot['stage']}-{shot['shape']}-{shot['theme']}.jpg"
        image = Image.open(src / "screenshots" / shot["image"]).convert("RGB")
        (out / "gallery/full").mkdir(parents=True, exist_ok=True)
        (out / "gallery/thumb").mkdir(parents=True, exist_ok=True)
        image.save(out / "gallery/full" / name, "JPEG", quality=86, optimize=True, progressive=True)
        full_size = image.size
        image.thumbnail((THUMB_WIDTH.get(shot["shape"], THUMB_DEFAULT_WIDTH), 1200))
        image.save(out / "gallery/thumb" / name, "JPEG", quality=62, optimize=True)
        stage["images"].setdefault(shot["shape"], {})[shot["theme"]] = {
            "src": f"gallery/thumb/{name}", "w": image.width, "h": image.height,
            "full": f"gallery/full/{name}", "fw": full_size[0], "fh": full_size[1]}
    order = ["desktop", "element", "as-is", "tablet-landscape", "tablet-portrait", "mobile-landscape", "mobile-portrait", "small-portrait"]
    result = []
    for stage in stages.values():
        stage["shapes"] = sorted(stage["images"], key=lambda s: order.index(s) if s in order else len(order))
        if stage.get("report"):
            # The manifest links the Playwright report relative to the screenshots folder.
            stage["report"] = re.sub(r"^\.\./", "reports/", stage["report"])
        result.append(stage)
    result.sort(key=lambda s: (s["group"], s["stage"]))
    return {"stages": result, "total": len([s for s in shots if not s.get("retry")])}


def badge(totals):
    if not totals or not totals[1]:
        return {"schemaVersion": 1, "label": "coverage", "message": "unknown", "color": "lightgrey"}
    pct = 100 * totals[0] / totals[1]
    color = "brightgreen" if pct >= 90 else "green" if pct >= 80 else "yellow" if pct >= 70 else "orange"
    return {"schemaVersion": 1, "label": "coverage", "message": f"{pct:.1f}%", "color": color}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--coverage", type=Path)
    parser.add_argument("--properties", type=Path)
    parser.add_argument("--fuzz", type=Path)
    parser.add_argument("--scale", type=Path)
    parser.add_argument("--sources", type=Path)
    parser.add_argument("--feature", default="zmq", help="the engine build whose property and fuzz runs are shown")
    parser.add_argument("--repo-url", default="https://github.com/OceanEilonwy/monokulo")
    args = parser.parse_args(argv)

    out = args.out
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    data = {"feature": args.feature, "repo": args.repo_url, "sources": load(args.sources) if args.sources else {}}
    if args.coverage:
        data["coverage"] = coverage(args.coverage, out)
        data["coverage"]["shipping"] = ship_totals(data["coverage"]["crates"])
        run_json = args.coverage / "stress/run.json"
        data["stress"] = stress(run_json) if run_json.is_file() else None
        data["gallery"] = gallery(args.coverage, out)
    if args.properties:
        data["properties"] = properties(args.properties, args.feature)
    if args.fuzz:
        data["fuzz"] = fuzz(args.fuzz, args.feature) or None
    if args.scale:
        run_json = next(iter(glob.glob(str(args.scale / "**/run.json"), recursive=True)), None)
        data["scale"] = stress(Path(run_json)) if run_json else None

    (out / "data.json").write_text(json.dumps(data, separators=(",", ":")))
    (out / "badge.json").write_text(json.dumps(badge((data.get("coverage") or {}).get("shipping"))))
    (out / "assets").mkdir()
    shutil.copy(THEME, out / "assets/theme.css")
    for font in FONTS:
        shutil.copy(font, out / "assets" / font.name)
    (out / "index.html").write_text(TEMPLATE.read_text().replace("@REPO_URL@", args.repo_url))
    (out / ".nojekyll").touch()
    have = [k for k in ("coverage", "properties", "fuzz", "scale") if data.get(k)]
    print(f"quality report in {out}: {', '.join(have) or 'no data'}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
