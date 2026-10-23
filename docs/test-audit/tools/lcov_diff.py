"""Summarise an llvm-cov JSON export, or diff it against the full-suite baseline.

llvm-cov's lcov export segfaults on this workspace, so this reads the JSON
export (segments + branches) and derives covered lines from the segments.
"""
import json
import re
import sys


def parse(path):
    with open(path) as f:
        data = json.load(f)
    lines, branches = set(), set()
    for export in data["data"]:
        for entry in export["files"]:
            name = entry["filename"]
            segs = entry.get("segments", [])
            # A line is covered when a counted segment active on it has a
            # non-zero count (llvm-cov's own line rule, approximately).
            for i, seg in enumerate(segs):
                line, _col, count, has_count = seg[0], seg[1], seg[2], seg[3]
                if not has_count or count == 0:
                    continue
                end = segs[i + 1][0] if i + 1 < len(segs) else line
                for no in range(line, max(end, line) + 1):
                    lines.add((name, no))
            for b in entry.get("branches", []):
                if b[4] > 0:
                    branches.add((name, b[0], b[1], "t"))
                if b[5] > 0:
                    branches.add((name, b[0], b[1], "f"))
    return lines, branches


def short(path):
    i = path.find("/crates/")
    return path[i + 1:] if i >= 0 else path


mode, report, baseline, filt, log = sys.argv[1:6]
ran = None
try:
    with open(log) as f:
        text = f.read()
except FileNotFoundError:
    text = ""
m = re.search(r"Summary \[.*?\]\s+(\d+) tests? run", text)
if m:
    ran = int(m.group(1))
failed = re.findall(r"^\s+FAIL \[.*?\] (.+)$", text, re.M)
try:
    lines, branches = parse(report)
except (FileNotFoundError, json.JSONDecodeError, KeyError):
    print(json.dumps({"mode": mode, "filter": filt, "error": "no report, see " + log}))
    sys.exit(0)
result = {"mode": mode, "filter": filt, "tests_run": ran, "failed": failed[:20], "log": log}
if mode == "alone":
    result.update(lines_covered=len(lines), branches_covered=len(branches))
else:
    base_lines, base_branches = parse(baseline)
    lost = sorted(base_lines - lines)
    by_file = {}
    for path, no in lost:
        by_file.setdefault(short(path), []).append(no)
    result.update(
        baseline_lines=len(base_lines),
        lines_lost=len(lost),
        pct_points_lost=round(100 * len(lost) / max(len(base_lines), 1), 3),
        branches_lost=len(base_branches - branches),
        lost_by_file={k: v[:40] for k, v in sorted(by_file.items(), key=lambda kv: -len(kv[1]))[:30]},
    )
print(json.dumps(result, indent=1))
