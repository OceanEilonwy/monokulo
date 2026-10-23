"""Load JUnit reports into the audit DB: load.py <db> <kind> <junit.xml> [suite-prefix]

kind: rust (nextest), browser (e2e Playwright), ui-browser (crates/ui Playwright),
node (node --test junit). Re-loading replaces duration/failed for known rows.

suite: rust = nextest binary id; Playwright = <prefix><project>:<spec file>;
node = <prefix><test file>.
"""
import os
import sqlite3
import sys
import xml.etree.ElementTree as ET

db, kind, path = sys.argv[1:4]
prefix = sys.argv[4] if len(sys.argv) > 4 else ""
repo = "/home/henry/Downloads/mokulo/.claude/worktrees/test-audit/"
con = sqlite3.connect(db)
root = ET.parse(path).getroot()


def cases():
    if kind == "node":
        for case in root.iter("testcase"):
            file = (case.get("file") or "").replace(repo, "")
            yield prefix + os.path.basename(file), file, case
        return
    for suite in root.iter("testsuite"):
        for case in suite.findall("testcase"):
            if kind == "rust":
                yield prefix + suite.get("name"), None, case
            else:
                spec = case.get("classname") or suite.get("name")
                base = "e2e/browser/tests/" if kind == "browser" else "crates/ui/browser/"
                yield f"{prefix}{suite.get('hostname')}:{spec}", base + spec, case


n = 0
for suite_name, file, case in cases():
    name = case.get("name")
    if case.find("skipped") is not None:
        continue
    fail = case.find("failure")
    if fail is None:
        fail = case.find("error")
    failed = 1 if fail is not None else 0
    note = None
    if failed:
        note = " | ".join((fail.get("message") or fail.text or "").strip().splitlines()[:3])[:500]
    ms = float(case.get("time") or 0) * 1000
    if kind == "rust":
        ttype = "property" if "::properties::" in name or "_properties::" in name else "unit"
    else:
        ttype = {"ui-browser": "component", "node": "unit"}.get(kind, "e2e")
    con.execute(
        """INSERT INTO test (name, suite, file, duration_ms, test_type, failed, failure_note)
           VALUES (?,?,?,?,?,?,?)
           ON CONFLICT(suite, name) DO UPDATE SET duration_ms=excluded.duration_ms,
             failed=excluded.failed, failure_note=excluded.failure_note""",
        (name, suite_name, file, ms, ttype, failed, note),
    )
    n += 1
con.commit()
print(f"loaded {n} from {path}")
