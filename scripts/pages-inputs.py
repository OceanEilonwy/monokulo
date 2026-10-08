#!/usr/bin/env python3
"""Downloads what the GitHub Pages site is built from: the newest artifacts on main.

    scripts/pages-inputs.py DIR

For each source it takes the newest finished run on main that still has the
artifacts (skipping cancelled runs; a failed run's results are shown, not
hidden), downloads them under DIR and writes DIR/sources.json naming the run
behind each (and a table of them to the job summary, in Actions). Needs the gh CLI with a token that can read Actions
(GH_TOKEN, and GITHUB_REPOSITORY or --repo).

    DIR/site/        the landing page and package repository (OpenWrt workflow); required
    DIR/coverage/    coverage-<sha> (CI)
    DIR/properties/  engine-properties-zmq-<run> (Engine property exploration, ZMQ build)
    DIR/fuzz/        engine-fuzz-<target>-zmq-<run>, one folder each (Engine fuzz exploration)
    DIR/scale/       engine-scale-measurements-<run> (Engine scale correctness, weekly)

Everything but the site is optional: the quality report says what it has
no data for. The site is required because Pages serves one deployment, so
publishing without it would take the package repository offline.
"""

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

# (key, workflow file, artifact name pattern, required, needs a successful run)
SOURCES = [
    ("site", "openwrt.yml", r"^monokulo-openwrt-site$", True, True),
    ("coverage", "ci.yml", r"^coverage-[0-9a-f]{40}$", False, False),
    ("properties", "engine-properties.yml", r"^engine-properties-zmq-\d+$", False, False),
    ("fuzz", "engine-fuzz.yml", r"^engine-fuzz-.+-zmq-\d+$", False, False),
    ("scale", "engine-scale.yml", r"^engine-scale-measurements-\d+$", False, False),
]
RUNS_TO_SEARCH = 30


def gh(*args):
    return subprocess.run(["gh", *args], check=True, capture_output=True, text=True).stdout


def api(repo, path):
    return json.loads(gh("api", f"repos/{repo}/{path}"))


def newest(repo, workflow, pattern, need_success):
    """The newest finished run on main with live artifacts matching `pattern`."""
    runs = api(repo, f"actions/workflows/{workflow}/runs?branch=main&status=completed&per_page={RUNS_TO_SEARCH}")["workflow_runs"]
    for run in runs:
        if run["conclusion"] in ("cancelled", "skipped") or (need_success and run["conclusion"] != "success"):
            continue
        artifacts = api(repo, f"actions/runs/{run['id']}/artifacts?per_page=100")["artifacts"]
        names = [a["name"] for a in artifacts if not a["expired"] and re.match(pattern, a["name"])]
        if names:
            return run, names
    return None, []


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("dir", type=Path)
    parser.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY"))
    args = parser.parse_args(argv)
    if not args.repo:
        sys.exit("name the repository with --repo or GITHUB_REPOSITORY")
    sources = {}
    for key, workflow, pattern, required, need_success in SOURCES:
        run, names = newest(args.repo, workflow, pattern, need_success)
        if not run:
            if required:
                sys.exit(f"{key}: no run of {workflow} on main in the last {RUNS_TO_SEARCH} still has its artifact; "
                         f"run that workflow on main (Actions > Run workflow), then this one again")
            print(f"{key}: no artifacts found, skipped", file=sys.stderr)
            continue
        for name in names:
            # One artifact goes straight in; several (the fuzz targets) get a folder each.
            dest = args.dir / key / (name if len(names) > 1 else "")
            gh("run", "download", str(run["id"]), "--repo", args.repo, "--name", name, "--dir", str(dest))
        sources[key] = {"run_id": run["id"], "sha": run["head_sha"], "date": run["updated_at"],
                        "conclusion": run["conclusion"], "artifacts": len(names)}
        print(f"{key}: run {run['id']} ({run['conclusion']}, {run['updated_at']}), {len(names)} artifact(s)", file=sys.stderr)
    (args.dir / "sources.json").write_text(json.dumps(sources, indent=2))
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a") as out:
            out.write("### Pages sources\n\n| Source | Run | Result | Finished |\n|---|---|---|---|\n")
            for key, run in sources.items():
                out.write(f"| {key} | [{run['run_id']}](https://github.com/{args.repo}/actions/runs/{run['run_id']}) | {run['conclusion']} | {run['date']} |\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
