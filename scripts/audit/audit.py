#!/usr/bin/env python3
"""The code audit's database and the commands agents use on it.

Setup (once, by whoever runs the audit):

    audit.py init                 make the database from schema.sql
    audit.py inventory            every Rust file at HEAD, with crate, size, blob
    audit.py modules              give each file a module (modules.toml)
    audit.py signals CLIPPY MACHETE   load tool output as hints
    audit.py plan                 make the reviews and their jobs

Agents (AUDIT_AGENT or --agent names who is asking):

    audit.py claim                take the next job and print its brief
    audit.py finding --job N ...  report a finding (makes a verify job)
    audit.py verify --job N ...   settle a verify job's finding
    audit.py note FINDING TEXT    add to a finding already recorded (yours or another's)
    audit.py amend FINDING --lines N-M [--file P]   correct where a finding points
    audit.py purpose PATH TEXT    say in one line what a file is for
    audit.py done --job N --summary TEXT
    audit.py release --job N      give a job back unfinished
    audit.py findings [--file P] [--module M] [--status S]
    audit.py query SQL            read-only SQL

Anyone:

    audit.py status
    audit.py report [--out FILE]

The database lives at target/code-audit/audit.db under the main checkout
(shared by every worktree), or wherever AUDIT_DB says.
"""

import argparse
import fnmatch
import json
import os
import re
import sqlite3
import subprocess
import sys
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = Path(
    subprocess.run(
        ["git", "rev-parse", "--show-toplevel"], cwd=HERE, capture_output=True, text=True, check=True
    ).stdout.strip()
)
EXCLUDED = ("e2e/", "crates/e2e-harness/")
LEASE_MINUTES = 90
# A review job's slice of a module: files are kept whole, and a slice grows
# until its non-test lines pass this.
SLICE_LINES = 4000

REQUIRED = {
    "finding": ("job", "file", "lines", "category", "severity", "confidence", "title", "problem",
                "scenario", "recommendation"),
    "verify": ("job", "verdict", "note"),
    "note": ("finding", "text"),
    "done": ("job", "summary"),
}

CATEGORIES = ("bug", "security", "type_safety", "simplify", "dead_code", "convention")
SEVERITIES = ("critical", "high", "medium", "low")
CONFIDENCES = ("high", "medium", "low")

CROSSCUTS = [
    (
        "money",
        "Money end to end: piconero (u64) arithmetic, `as` casts, checked versus wrapping operations, "
        "fiat to XMR conversion and rounding, amounts parsed from users, nodes and rate providers, "
        "and whether one amount type is used everywhere (shared::xmr_amount) or raw integers leak through.",
    ),
    (
        "payment-lifecycle",
        "An order's life across crates: engine store status, engine events, monokulo engine_view, "
        "checkout page, webhooks and the API. Look for states one side can reach that another can't "
        "represent, transitions that skip or repeat, and enums duplicated instead of shared.",
    ),
    (
        "concurrency",
        "Locks, async tasks and cancellation: lock ordering, holding a lock across .await, blocking "
        "calls inside async code, tasks spawned without supervision or shutdown, SQLite busy handling, "
        "and races between the engine's loops.",
    ),
    (
        "type-safety",
        "Types that would make bugs impossible: stringly-typed IDs and keys (compare shared::ids), "
        "bool parameters, Option<Option<_>>, tuples standing in for structs, errors as String or anyhow "
        "where callers need to match, newtypes that exist but are bypassed.",
    ),
    (
        "privacy-tor",
        "Every outbound connection monokulo and the engine make: which HTTP client, whether it goes over "
        "Tor when it should, DNS leaks, identifying headers (User-Agent, Referer), and anything that "
        "could link a merchant's onion service to a clearnet identity. Leaks are critical.",
    ),
    (
        "authz-boundary",
        "Authorization on every route in monokulo and the engine: which middleware guards which router, "
        "store scoping (one store's member or key must never reach another store), admin-only routes, "
        "and the boundary that only monokulo talks to the engine.",
    ),
    (
        "duplication",
        "The same logic written more than once across crates: SQLite helpers, rate limiting, settings "
        "parsing, HTTP clients, time and amount formatting, test support. Recommend the one place it "
        "should live.",
    ),
    (
        "dead-code-wiring",
        "Code only tests call, public items nothing uses, checks that are defined but never called "
        "(a wiring bug), settings nobody reads, feature flags and cfgs that are always on or off.",
    ),
    (
        "error-handling",
        "unwrap, expect and panic on paths that input can reach (see clippy signals), errors swallowed "
        "with `let _ =` or `.ok()`, errors that leak internals to users or secrets to logs, and retry "
        "loops without bounds.",
    ),
    (
        "house-rules",
        "The project's own rules: every setting in the live-settings registry (TOML options + CLI, "
        "runtime switches in DB, secrets env-only, invalid values stop the process); no cfg(test) value "
        "swaps (tunables via ScanTuning::with_tuning); colours only in theme roles; every select is "
        "<mk-select> with controls::Choice; tests that use HTTP say over_http in the name.",
    ),
]


def db_path():
    if os.environ.get("AUDIT_DB"):
        return Path(os.environ["AUDIT_DB"])
    common = subprocess.run(
        ["git", "rev-parse", "--path-format=absolute", "--git-common-dir"],
        cwd=HERE, capture_output=True, text=True, check=True,
    ).stdout.strip()
    return Path(common).parent / "target" / "code-audit" / "audit.db"


def connect(readonly=False):
    path = db_path()
    if not path.exists():
        sys.exit(f"no audit database at {path}; run `audit.py init` first")
    if readonly:
        conn = sqlite3.connect(f"file:{path}?mode=ro", uri=True, timeout=60)
    else:
        conn = sqlite3.connect(path, timeout=60, isolation_level=None)
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA busy_timeout = 60000")
    conn.execute("PRAGMA foreign_keys = ON")
    return conn


class Tx:
    """BEGIN IMMEDIATE, so two agents claiming at once take turns."""

    def __init__(self, conn):
        self.conn = conn

    def __enter__(self):
        self.conn.execute("BEGIN IMMEDIATE")
        return self.conn

    def __exit__(self, kind, *_):
        self.conn.execute("COMMIT" if kind is None else "ROLLBACK")


def git(*args):
    return subprocess.run(["git", *args], cwd=REPO, capture_output=True, text=True, check=True).stdout


def agent_name(args):
    name = args.agent or os.environ.get("AUDIT_AGENT")
    if not name:
        sys.exit("say who you are: --agent NAME or AUDIT_AGENT=NAME")
    return name


def file_id(conn, path):
    path = path.removeprefix("./")
    if path.startswith(str(REPO)):
        path = str(Path(path).relative_to(REPO))
    row = conn.execute("SELECT id, loc FROM crate_file WHERE path = ?", (path,)).fetchone()
    if row is None:
        sys.exit(f"{path} is not an audited file (paths are relative to the repository root)")
    return row


# --- setup -------------------------------------------------------------------


def cmd_init(args):
    path = db_path()
    if path.exists():
        sys.exit(f"{path} exists already; move it aside to start over")
    path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(path, isolation_level=None)
    conn.execute("PRAGMA journal_mode = WAL")
    conn.executescript((HERE / "schema.sql").read_text())
    print(f"made {path}")


def crate_root(path):
    for parent in Path(path).parents:
        if (REPO / parent / "Cargo.toml").exists():
            return parent
    return Path(".")


def crate_description(root):
    manifest = tomllib.loads((REPO / root / "Cargo.toml").read_text())
    package = manifest.get("package", {})
    if package.get("description"):
        return package["name"], package["description"]
    for entry in ("src/lib.rs", "src/main.rs"):
        source = REPO / root / entry
        if source.exists():
            lines = []
            for line in source.read_text().splitlines():
                if not line.startswith("//!"):
                    if lines:
                        break
                    continue
                text = line.removeprefix("//!").strip()
                if not text and lines:
                    break
                lines.append(text)
            if lines:
                return package.get("name", root.name), " ".join(lines)
    return package.get("name", root.name), ""


TEST_NAME = re.compile(r"(^|/)(tests?|test_support|.*_tests?|.*_properties|property_support|test_log|test_chain)\.rs$")


def is_test(path):
    return "/tests/" in path or path.startswith("fuzz/tests/") or bool(TEST_NAME.search(path))


def cmd_inventory(args):
    conn = connect()
    commit = git("rev-parse", "HEAD").strip()
    with Tx(conn):
        conn.execute("INSERT OR REPLACE INTO meta VALUES ('commit', ?)", (commit,))
        for line in git("ls-files", "-s", "--", "*.rs").splitlines():
            _mode, blob, _stage, path = line.split(maxsplit=3)
            if path.startswith(EXCLUDED):
                continue
            root = crate_root(path)
            name, description = crate_description(root)
            conn.execute(
                "INSERT INTO crate (name, path, description) VALUES (?, ?, ?) "
                "ON CONFLICT (path) DO UPDATE SET description = excluded.description",
                (name, str(root), description),
            )
            crate = conn.execute("SELECT id FROM crate WHERE path = ?", (str(root),)).fetchone()["id"]
            loc = sum(1 for _ in open(REPO / path, encoding="utf-8"))
            conn.execute(
                "INSERT INTO crate_file (crate_id, path, loc, blob_sha, is_test) VALUES (?, ?, ?, ?, ?) "
                "ON CONFLICT (path) DO UPDATE SET loc = excluded.loc, blob_sha = excluded.blob_sha",
                (crate, path, loc, blob, int(is_test(path))),
            )
        conn.execute(
            "UPDATE crate SET loc = (SELECT coalesce(sum(loc), 0) FROM crate_file WHERE crate_id = crate.id)"
        )
    files, lines = conn.execute("SELECT count(*), sum(loc) FROM crate_file").fetchone()
    print(f"{files} files, {lines} lines, {conn.execute('SELECT count(*) FROM crate').fetchone()[0]} crates at {commit[:9]}")


def cmd_modules(args):
    conn = connect()
    spec = tomllib.loads((HERE / "modules.toml").read_text())["module"]
    files = [r["path"] for r in conn.execute("SELECT path FROM crate_file ORDER BY path")]
    primary = {}
    for module in spec:
        for path in files:
            if path not in primary and any(fnmatch.fnmatch(path, g) for g in module["files"]):
                primary[path] = module["name"]
    missing = [p for p in files if p not in primary]
    if missing:
        sys.exit("files in no module (add them to modules.toml):\n  " + "\n  ".join(missing))
    with Tx(conn):
        conn.execute("DELETE FROM module_file")
        for module in spec:
            conn.execute(
                "INSERT INTO module (name, description, focus, priority) VALUES (?, ?, ?, ?) "
                "ON CONFLICT (name) DO UPDATE SET description = excluded.description, "
                "focus = excluded.focus, priority = excluded.priority",
                (module["name"], module["description"], module.get("focus", ""), module.get("priority", 2)),
            )
            mid = conn.execute("SELECT id FROM module WHERE name = ?", (module["name"],)).fetchone()["id"]
            for path in files:
                fid = conn.execute("SELECT id FROM crate_file WHERE path = ?", (path,)).fetchone()["id"]
                if primary[path] == module["name"]:
                    conn.execute("INSERT INTO module_file VALUES (?, ?, 1)", (mid, fid))
                elif any(fnmatch.fnmatch(path, g) for g in module.get("context", [])):
                    conn.execute("INSERT INTO module_file VALUES (?, ?, 0)", (mid, fid))
        conn.execute(
            "UPDATE crate_file SET is_tooling = 1 WHERE id IN (SELECT crate_file_id FROM module_file "
            "JOIN module ON module.id = module_id WHERE is_primary AND module.priority = 3)"
        )
    for row in conn.execute(
        "SELECT m.name, count(*) files, sum(cf.loc) loc, sum(CASE WHEN cf.is_test THEN 0 ELSE cf.loc END) code "
        "FROM module m JOIN module_file mf ON mf.module_id = m.id AND mf.is_primary "
        "JOIN crate_file cf ON cf.id = mf.crate_file_id GROUP BY m.id ORDER BY m.priority, m.name"
    ):
        print(f"{row['name']:32} {row['files']:4} files {row['code']:7} code {row['loc'] - row['code']:7} test")


def cmd_signals(args):
    conn = connect()
    with Tx(conn):
        conn.execute("DELETE FROM signal")
        seen = set()
        for line in open(args.clippy, encoding="utf-8"):
            message = json.loads(line)
            if message.get("reason") != "compiler-message":
                continue
            diag = message["message"]
            spans = [s for s in diag.get("spans", []) if s.get("is_primary")]
            if not diag.get("code") or not spans:
                continue
            span = spans[0]
            path = str(Path(span["file_name"]).resolve().relative_to(REPO)) if span["file_name"].startswith("/") else span["file_name"]
            key = (path, span["line_start"], diag["code"]["code"])
            if key in seen:
                continue
            seen.add(key)
            row = conn.execute("SELECT id FROM crate_file WHERE path = ?", (path,)).fetchone()
            if row is None:
                continue
            conn.execute(
                "INSERT INTO signal (crate_file_id, line, tool, code, message) VALUES (?, ?, 'clippy', ?, ?)",
                (row["id"], span["line_start"], diag["code"]["code"], diag["message"]),
            )
        if args.machete:
            crate = None
            for line in open(args.machete, encoding="utf-8"):
                if " -- " in line:
                    crate = line.split(" -- ")[0].strip()
                elif line.startswith("\t") and crate:
                    conn.execute(
                        "INSERT INTO signal (crate_file_id, line, tool, code, message) VALUES "
                        "((SELECT cf.id FROM crate_file cf JOIN crate c ON c.id = cf.crate_id WHERE c.name = ? "
                        "ORDER BY cf.path LIKE '%/lib.rs' DESC, cf.path LIKE '%/main.rs' DESC LIMIT 1), NULL, "
                        "'cargo-machete', 'unused_dependency', ?)",
                        (crate, f"{crate}'s Cargo.toml lists {line.strip()} but nothing uses it"),
                    )
    print(f"{conn.execute('SELECT count(*) FROM signal').fetchone()[0]} signals")


def cmd_plan(args):
    conn = connect()
    if conn.execute("SELECT count(*) FROM review").fetchone()[0]:
        sys.exit("reviews are planned already")
    commit = conn.execute("SELECT value FROM meta WHERE key = 'commit'").fetchone()["value"]
    with Tx(conn):
        for module in conn.execute("SELECT * FROM module ORDER BY priority, name").fetchall():
            files = conn.execute(
                "SELECT cf.id, cf.path, cf.loc, cf.is_test, mf.is_primary FROM module_file mf "
                "JOIN crate_file cf ON cf.id = mf.crate_file_id WHERE mf.module_id = ? ORDER BY cf.path",
                (module["id"],),
            ).fetchall()
            targets = [f for f in files if f["is_primary"] and not f["is_test"]]
            context = [f for f in files if not f["is_primary"] or f["is_test"]]
            slices, current, size = [], [], 0
            for f in targets:
                if current and size + f["loc"] > SLICE_LINES:
                    slices.append(current)
                    current, size = [], 0
                current.append(f)
                size += f["loc"]
            if current:
                slices.append(current)
            for n, chunk in enumerate(slices, 1):
                title = module["name"] if len(slices) == 1 else f"{module['name']} ({n}/{len(slices)})"
                rid = conn.execute(
                    "INSERT INTO review (module_id, kind, title, commit_sha) VALUES (?, 'module', ?, ?)",
                    (module["id"], title, commit),
                ).lastrowid
                for f in chunk:
                    conn.execute("INSERT INTO review_file VALUES (?, ?, 'target')", (rid, f["id"]))
                for f in context:
                    conn.execute("INSERT INTO review_file VALUES (?, ?, 'context')", (rid, f["id"]))
                conn.execute(
                    "INSERT INTO job (kind, review_id, priority) VALUES ('review', ?, ?)",
                    (rid, module["priority"]),
                )
        for name, brief in CROSSCUTS:
            rid = conn.execute(
                "INSERT INTO review (kind, title, brief, commit_sha) VALUES ('crosscut', ?, ?, ?)",
                (name, brief, commit),
            ).lastrowid
            conn.execute("INSERT INTO job (kind, review_id, priority) VALUES ('crosscut', ?, 2)", (rid,))
    cmd_status(args)


# --- agents ------------------------------------------------------------------


def take(conn, agent):
    conn.execute(
        "UPDATE job SET status = 'pending', agent = NULL, lease_until = NULL "
        "WHERE status = 'claimed' AND lease_until < datetime('now')"
    )
    held = conn.execute(
        "SELECT id FROM job WHERE status = 'claimed' AND agent = ?", (agent,)
    ).fetchone()
    if held:
        return held["id"], True
    reviews_open = conn.execute(
        "SELECT count(*) FROM job WHERE kind = 'review' AND status != 'done'"
    ).fetchone()[0]
    row = conn.execute(
        "SELECT id FROM job WHERE status = 'pending' AND coalesce(not_agent, '') != ? "
        "AND (kind != 'crosscut' OR ? = 0) "
        "ORDER BY CASE kind WHEN 'verify' THEN 0 WHEN 'review' THEN 1 ELSE 2 END, priority, id LIMIT 1",
        (agent, reviews_open),
    ).fetchone()
    if row is None:
        return None, False
    conn.execute(
        "UPDATE job SET status = 'claimed', agent = ?, attempts = attempts + 1, "
        f"lease_until = datetime('now', '+{LEASE_MINUTES} minutes') WHERE id = ?",
        (agent, row["id"]),
    )
    return row["id"], False


def cmd_claim(args):
    agent = agent_name(args)
    conn = connect()
    with Tx(conn):
        job_id, held = take(conn, agent)
    if job_id is None:
        left = conn.execute("SELECT count(*) FROM job WHERE status != 'done'").fetchone()[0]
        held = conn.execute(
            "SELECT count(*) FROM job WHERE kind = 'review' AND status = 'claimed'"
        ).fetchone()[0]
        why = (f" Crosscut jobs open once the {held} module reviews other agents hold are done; "
               "try again in a few minutes." if held else "")
        print(f"NO JOB for {agent}: {left} jobs not done, none you can take now.{why}")
        return
    if held:
        print(f"(you hold job {job_id} already; finish it with `done` or give it back with `release`)\n")
    print_brief(conn, job_id)


def print_brief(conn, job_id):
    job = conn.execute("SELECT * FROM job WHERE id = ?", (job_id,)).fetchone()
    print(f"JOB {job['id']} ({job['kind']})")
    if job["kind"] == "verify":
        f = conn.execute(
            "SELECT rf.*, cf.path FROM review_finding rf JOIN crate_file cf ON cf.id = rf.crate_file_id "
            "WHERE rf.id = ?", (job["finding_id"],)
        ).fetchone()
        print(f"Verify finding {f['id']} by {f['reported_by']}: {f['path']}:{f['line_start']}-{f['line_end']}")
        for key in ("category", "severity", "confidence", "title", "problem", "failure_scenario",
                    "recommendation", "regression_test"):
            print(f"  {key}: {f[key]}")
        similar = conn.execute(
            "SELECT id, line_start, title, status FROM review_finding WHERE crate_file_id = ? AND id != ? "
            "AND status != 'rejected' ORDER BY line_start", (f["crate_file_id"], f["id"])
        ).fetchall()
        if similar:
            print("Other findings in this file (is it a duplicate?):")
            for s in similar:
                print(f"  #{s['id']} line {s['line_start']} [{s['status']}] {s['title']}")
        return
    review = conn.execute(
        "SELECT r.*, m.name module, m.description, m.focus FROM review r LEFT JOIN module m ON m.id = r.module_id "
        "WHERE r.id = ?", (job["review_id"],)
    ).fetchone()
    print(f"Review {review['id']}: {review['title']} at commit {review['commit_sha'][:9]}")
    if review["kind"] == "module":
        print(f"Module: {review['description']}")
        print(f"Look hardest at: {review['focus']}")
    else:
        print(f"Theme: {review['brief']}")
        print("Scope: the whole audited workspace. Search for the theme; read what it leads you to.")
        modules = conn.execute(
            "SELECT m.name, count(rf.id) n FROM module m LEFT JOIN review r ON r.module_id = m.id "
            "LEFT JOIN review_finding rf ON rf.review_id = r.id AND rf.status != 'rejected' "
            "GROUP BY m.id ORDER BY m.priority, m.name"
        ).fetchall()
        print("Findings so far per module: " + ", ".join(f"{m['name']} {m['n']}" for m in modules))
        return
    rows = conn.execute(
        "SELECT cf.id, cf.path, cf.loc, cf.purpose, rf.role FROM review_file rf "
        "JOIN crate_file cf ON cf.id = rf.crate_file_id WHERE rf.review_id = ? ORDER BY rf.role DESC, cf.path",
        (review["id"],),
    ).fetchall()
    print("\nReview these files (read them in full):")
    for r in rows:
        if r["role"] == "target":
            print(f"  {r['path']} ({r['loc']} lines)")
    context = [r for r in rows if r["role"] == "context"]
    if context:
        print("\nContext (tests and helpers; search them, don't review them):")
        for r in context:
            print(f"  {r['path']} ({r['loc']} lines)")
    signals = conn.execute(
        "SELECT cf.path, s.line, s.code, s.message FROM signal s JOIN crate_file cf ON cf.id = s.crate_file_id "
        "JOIN review_file rf ON rf.crate_file_id = cf.id AND rf.review_id = ? AND rf.role = 'target' "
        "ORDER BY cf.path, s.line", (review["id"],)
    ).fetchall()
    if signals:
        print(f"\nTool signals in these files ({len(signals)}; triage them, report only real problems):")
        for s in signals:
            print(f"  {s['path']}:{s['line'] or '-'} {s['code']}: {s['message']}")
    existing = conn.execute(
        "SELECT rf.id, cf.path, rf.line_start, rf.status, rf.title FROM review_finding rf "
        "JOIN crate_file cf ON cf.id = rf.crate_file_id JOIN review_file x ON x.crate_file_id = cf.id "
        "AND x.review_id = ? AND x.role = 'target' ORDER BY cf.path, rf.line_start", (review["id"],)
    ).fetchall()
    if existing:
        print("\nAlready reported in these files (don't repeat them):")
        for e in existing:
            print(f"  #{e['id']} {e['path']}:{e['line_start']} [{e['status']}] {e['title']}")


def own_job(conn, job_id, agent, kinds):
    job = conn.execute("SELECT * FROM job WHERE id = ?", (job_id,)).fetchone()
    if job is None or job["agent"] != agent or job["status"] != "claimed":
        sys.exit(f"job {job_id} is not claimed by {agent}")
    if job["kind"] not in kinds:
        sys.exit(f"job {job_id} is a {job['kind']} job")
    conn.execute(
        f"UPDATE job SET lease_until = datetime('now', '+{LEASE_MINUTES} minutes') WHERE id = ?", (job_id,)
    )
    return job


def cmd_finding(args):
    agent = agent_name(args)
    conn = connect()
    start, _, end = str(args.lines).partition("-")
    if not (start.isdigit() and (end.isdigit() or not end)):
        sys.exit(f"--lines takes one range, START or START-END, not {args.lines!r}; "
                 "record separate places as separate findings or name them in --problem")
    start, end = int(start), int(end or start)
    with Tx(conn):
        job = own_job(conn, args.job, agent, ("review", "crosscut"))
        f = file_id(conn, args.file)
        if not 1 <= start <= end <= f["loc"]:
            sys.exit(f"lines {args.lines} are outside {args.file} (1-{f['loc']}); "
                     "take line numbers from that one file at the audited commit")
        fid = conn.execute(
            "INSERT INTO review_finding (review_id, crate_file_id, line_start, line_end, category, severity, "
            "confidence, title, problem, failure_scenario, recommendation, regression_test, reported_by) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (job["review_id"], f["id"], start, end, args.category, args.severity, args.confidence, args.title,
             args.problem, args.scenario, args.recommendation, args.test or "", agent),
        ).lastrowid
        priority = SEVERITIES.index(args.severity)
        conn.execute(
            "INSERT INTO job (kind, finding_id, priority, not_agent) VALUES ('verify', ?, ?, ?)",
            (fid, priority, agent),
        )
    print(f"finding {fid} recorded; a verify job is queued for another agent")


def cmd_verify(args):
    agent = agent_name(args)
    conn = connect()
    with Tx(conn):
        job = own_job(conn, args.job, agent, ("verify",))
        if args.verdict == "duplicate" and not args.duplicate_of:
            sys.exit("a duplicate needs --duplicate-of FINDING")
        updates = {"status": args.verdict, "verified_by": agent, "verify_note": args.note,
                   "duplicate_of": args.duplicate_of}
        if args.severity:
            updates["severity"] = args.severity
        sets = ", ".join(f"{k} = ?" for k in updates)
        conn.execute(f"UPDATE review_finding SET {sets} WHERE id = ?", (*updates.values(), job["finding_id"]))
        conn.execute("UPDATE job SET status = 'done', done_at = datetime('now') WHERE id = ?", (job["id"],))
    print(f"finding {job['finding_id']} is {args.verdict}")


def cmd_note(args):
    agent = agent_name(args)
    conn = connect()
    with Tx(conn):
        row = conn.execute("SELECT problem FROM review_finding WHERE id = ?", (args.finding,)).fetchone()
        if row is None:
            sys.exit(f"no finding {args.finding}")
        conn.execute(
            "UPDATE review_finding SET problem = ? WHERE id = ?",
            (f"{row['problem']}\n\nAdded by {agent}: {args.text}", args.finding),
        )
    print(f"added to finding {args.finding}")


def cmd_amend(args):
    agent = agent_name(args)
    conn = connect()
    start, _, end = str(args.lines).partition("-")
    if not (start.isdigit() and (end.isdigit() or not end)):
        sys.exit(f"--lines takes one range, START or START-END, not {args.lines!r}")
    start, end = int(start), int(end or start)
    with Tx(conn):
        row = conn.execute(
            "SELECT rf.crate_file_id, cf.path FROM review_finding rf JOIN crate_file cf ON cf.id = rf.crate_file_id "
            "WHERE rf.id = ?", (args.finding,)
        ).fetchone()
        if row is None:
            sys.exit(f"no finding {args.finding}")
        f = file_id(conn, args.file) if args.file else file_id(conn, row["path"])
        if not 1 <= start <= end <= f["loc"]:
            sys.exit(f"lines {args.lines} are outside the file (1-{f['loc']})")
        conn.execute(
            "UPDATE review_finding SET crate_file_id = ?, line_start = ?, line_end = ?, "
            "problem = problem || ? WHERE id = ?",
            (f["id"], start, end, f"\n\n{agent} moved this to {args.file or row['path']}:{start}-{end}.", args.finding),
        )
    print(f"finding {args.finding} now points at lines {start}-{end}")


def cmd_purpose(args):
    conn = connect()
    with Tx(conn):
        f = file_id(conn, args.path)
        conn.execute("UPDATE crate_file SET purpose = ? WHERE id = ?", (args.text, f["id"]))
    print("noted")


def cmd_done(args):
    agent = agent_name(args)
    conn = connect()
    with Tx(conn):
        job = own_job(conn, args.job, agent, ("review", "crosscut"))
        if job["kind"] == "review":
            blank = conn.execute(
                "SELECT cf.path FROM review_file rf JOIN crate_file cf ON cf.id = rf.crate_file_id "
                "WHERE rf.review_id = ? AND rf.role = 'target' AND cf.purpose = ''", (job["review_id"],)
            ).fetchall()
            if blank:
                sys.exit("say what these files are for first (`purpose PATH TEXT`):\n  "
                         + "\n  ".join(b["path"] for b in blank))
        conn.execute("UPDATE review SET summary = ? WHERE id = ?", (args.summary, job["review_id"]))
        conn.execute("UPDATE job SET status = 'done', done_at = datetime('now') WHERE id = ?", (job["id"],))
    print(f"job {job['id']} done")


def cmd_release(args):
    agent = agent_name(args)
    conn = connect()
    with Tx(conn):
        own_job(conn, args.job, agent, ("review", "crosscut", "verify"))
        conn.execute(
            "UPDATE job SET status = 'pending', agent = NULL, lease_until = NULL WHERE id = ?", (args.job,)
        )
    print(f"job {args.job} is back in the queue")


def cmd_findings(args):
    conn = connect(readonly=True)
    where, params = [], []
    if args.file:
        where.append("cf.path = ?")
        params.append(args.file)
    if args.module:
        where.append("m.name = ?")
        params.append(args.module)
    if args.status:
        where.append("rf.status = ?")
        params.append(args.status)
    sql = (
        "SELECT rf.id, rf.status, rf.severity, rf.category, cf.path, rf.line_start, rf.title "
        "FROM review_finding rf JOIN crate_file cf ON cf.id = rf.crate_file_id "
        "JOIN review r ON r.id = rf.review_id LEFT JOIN module m ON m.id = r.module_id"
        + (" WHERE " + " AND ".join(where) if where else "") + " ORDER BY cf.path, rf.line_start"
    )
    for r in conn.execute(sql, params):
        print(f"#{r['id']} [{r['status']}/{r['severity']}/{r['category']}] {r['path']}:{r['line_start']} {r['title']}")


def cmd_query(args):
    conn = connect(readonly=True)
    try:
        cursor = conn.execute(args.sql)
    except sqlite3.Error as error:
        tables = conn.execute(
            "SELECT name FROM sqlite_master WHERE type IN ('table', 'view') ORDER BY name"
        ).fetchall()
        lines = [f"query failed: {error}", "tables and their columns:"]
        for t in tables:
            columns = [c["name"] for c in conn.execute(f"PRAGMA table_info({t['name']})")]
            lines.append(f"  {t['name']}: {', '.join(columns)}")
        sys.exit("\n".join(lines))
    names = [d[0] for d in cursor.description or []]
    print("\t".join(names))
    for row in cursor:
        print("\t".join("" if v is None else str(v) for v in row))


# --- anyone ------------------------------------------------------------------


def cmd_status(args):
    conn = connect(readonly=True)
    print("jobs:")
    for r in conn.execute(
        "SELECT kind, status, count(*) n FROM job GROUP BY kind, status ORDER BY kind, status"
    ):
        print(f"  {r['kind']:9} {r['status']:8} {r['n']}")
    for r in conn.execute(
        "SELECT agent, id, kind, lease_until FROM job WHERE status = 'claimed' ORDER BY agent"
    ):
        print(f"  {r['agent']} holds job {r['id']} ({r['kind']}) until {r['lease_until']}")
    print("findings:")
    for r in conn.execute(
        "SELECT status, severity, count(*) n FROM review_finding GROUP BY status, severity "
        "ORDER BY status, CASE severity WHEN 'critical' THEN 0 WHEN 'high' THEN 1 WHEN 'medium' THEN 2 ELSE 3 END"
    ):
        print(f"  {r['status']:9} {r['severity']:8} {r['n']}")


def cmd_report(args):
    conn = connect(readonly=True)
    commit = conn.execute("SELECT value FROM meta WHERE key = 'commit'").fetchone()["value"]
    out = [f"# Code audit at {commit[:9]}", ""]
    rows = conn.execute(
        "SELECT rf.*, cf.path, coalesce(m.name, 'crosscut: ' || r.title) area FROM review_finding rf "
        "JOIN crate_file cf ON cf.id = rf.crate_file_id JOIN review r ON r.id = rf.review_id "
        "LEFT JOIN module m ON m.id = r.module_id WHERE rf.status IN ('confirmed', 'open') "
        "ORDER BY CASE rf.severity WHEN 'critical' THEN 0 WHEN 'high' THEN 1 WHEN 'medium' THEN 2 ELSE 3 END, "
        "rf.status, area, cf.path, rf.line_start"
    ).fetchall()
    for severity in SEVERITIES:
        group = [r for r in rows if r["severity"] == severity]
        if not group:
            continue
        out += [f"## {severity.capitalize()} ({len(group)})", ""]
        for r in group:
            flag = "" if r["status"] == "confirmed" else " (not yet verified)"
            out += [
                f"### #{r['id']} {r['title']}{flag}",
                f"`{r['path']}:{r['line_start']}` · {r['category']} · {r['area']} · confidence {r['confidence']}",
                "",
                r["problem"],
                "",
                f"**Failure:** {r['failure_scenario']}",
                "",
                f"**Fix:** {r['recommendation']}",
            ]
            if r["regression_test"]:
                out += ["", f"**Test:** {r['regression_test']}"]
            if r["verify_note"]:
                out += ["", f"**Verifier ({r['verified_by']}):** {r['verify_note']}"]
            out.append("")
    text = "\n".join(out)
    if args.out:
        Path(args.out).write_text(text)
        print(f"wrote {args.out} ({len(rows)} findings)")
    else:
        print(text)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--agent")
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("init", parents=[common]).set_defaults(run=cmd_init)
    sub.add_parser("inventory", parents=[common]).set_defaults(run=cmd_inventory)
    sub.add_parser("modules", parents=[common]).set_defaults(run=cmd_modules)
    p = sub.add_parser("signals", parents=[common])
    p.add_argument("clippy")
    p.add_argument("machete", nargs="?")
    p.set_defaults(run=cmd_signals)
    sub.add_parser("plan", parents=[common]).set_defaults(run=cmd_plan)
    sub.add_parser("claim", parents=[common]).set_defaults(run=cmd_claim)

    p = sub.add_parser("finding", parents=[common])
    p.add_argument("--from", dest="from_file", help="read these fields from a JSON object in FILE")
    p.add_argument("--job", type=int)
    p.add_argument("--file")
    p.add_argument("--lines", help="START or START-END")
    p.add_argument("--category", choices=CATEGORIES)
    p.add_argument("--severity", choices=SEVERITIES)
    p.add_argument("--confidence", choices=CONFIDENCES)
    p.add_argument("--title")
    p.add_argument("--problem")
    p.add_argument("--scenario", help="concrete input or state, and the wrong outcome")
    p.add_argument("--recommendation")
    p.add_argument("--test", help="the regression test that would catch it")
    p.set_defaults(run=cmd_finding)

    p = sub.add_parser("verify", parents=[common])
    p.add_argument("--from", dest="from_file", help="read these fields from a JSON object in FILE")
    p.add_argument("--job", type=int)
    p.add_argument("--verdict", choices=("confirmed", "rejected", "duplicate"))
    p.add_argument("--note")
    p.add_argument("--duplicate-of", type=int)
    p.add_argument("--severity", choices=SEVERITIES, help="correct the severity")
    p.set_defaults(run=cmd_verify)

    p = sub.add_parser("note", parents=[common])
    p.add_argument("--from", dest="from_file", help="read finding and text from a JSON object in FILE")
    p.add_argument("finding", type=int, nargs="?")
    p.add_argument("text", nargs="?")
    p.set_defaults(run=cmd_note)

    p = sub.add_parser("amend", parents=[common])
    p.add_argument("finding", type=int)
    p.add_argument("--lines", required=True)
    p.add_argument("--file", help="move the finding to another file")
    p.set_defaults(run=cmd_amend)

    p = sub.add_parser("purpose", parents=[common])
    p.add_argument("path")
    p.add_argument("text")
    p.set_defaults(run=cmd_purpose)

    p = sub.add_parser("done", parents=[common])
    p.add_argument("--from", dest="from_file", help="read job and summary from a JSON object in FILE")
    p.add_argument("--job", type=int)
    p.add_argument("--summary")
    p.set_defaults(run=cmd_done)

    p = sub.add_parser("release", parents=[common])
    p.add_argument("--job", type=int, required=True)
    p.set_defaults(run=cmd_release)

    p = sub.add_parser("findings", parents=[common])
    p.add_argument("--file")
    p.add_argument("--module")
    p.add_argument("--status")
    p.set_defaults(run=cmd_findings)

    p = sub.add_parser("query", parents=[common])
    p.add_argument("sql")
    p.set_defaults(run=cmd_query)

    sub.add_parser("status", parents=[common]).set_defaults(run=cmd_status)
    p = sub.add_parser("report", parents=[common])
    p.add_argument("--out")
    p.set_defaults(run=cmd_report)

    args = parser.parse_args()
    if getattr(args, "from_file", None):
        fields = json.loads(Path(args.from_file).read_text())
        for key, value in fields.items():
            key = key.replace("-", "_")
            key = {"failure_scenario": "scenario", "regression_test": "test", "agent": "agent"}.get(key, key)
            if not hasattr(args, key):
                sys.exit(f"{args.from_file}: {args.command} has no field {key!r}")
            setattr(args, key, value)
    missing = [k for k in REQUIRED.get(args.command, ()) if getattr(args, k, None) in (None, "")]
    if missing:
        sys.exit(f"{args.command} needs: " + ", ".join(m.replace("_", "-") for m in missing))
    for key, allowed in (("category", CATEGORIES), ("severity", SEVERITIES), ("confidence", CONFIDENCES),
                         ("verdict", ("confirmed", "rejected", "duplicate"))):
        value = getattr(args, key, None)
        if value is not None and value not in allowed:
            sys.exit(f"{key} must be one of {', '.join(allowed)}, not {value!r}")
    args.run(args)


if __name__ == "__main__":
    main()
