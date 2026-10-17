#!/usr/bin/env python3
"""Catalog the workspace's first-level Rust dependencies into SQLite.

Only dependencies a workspace crate names in its own Cargo.toml are listed;
their own dependencies are never catalogued as dependencies, only counted as
the transitive weight each first-level one brings (`dependency_weight`).

    python3 scripts/dependency-catalog.py [docs/dependencies/catalog.sqlite]
        [--bloat BINARY=bloat.json ...]

The review of whether to drop each dependency lives in
docs/dependencies/proposals.json, written by hand (or by an agent), and is
loaded into the `proposals` table on every run, ranked least used first.

`--bloat` takes `cargo bloat --release --crates -n 0 --message-format json`
output for a shipped binary and records each dependency's share of its
`.text`; without it, the sizes measured last time are kept.
"""

import argparse
import json
import os
import re
import sqlite3
import subprocess
import sys
from collections import defaultdict

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

SCHEMA = """
CREATE TABLE IF NOT EXISTS crates (
    name TEXT PRIMARY KEY,
    path TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS dependencies (
    name TEXT PRIMARY KEY,
    source TEXT NOT NULL,
    resolved_version TEXT,
    description TEXT,
    repository TEXT
);
-- One row per `[dependencies]`-style entry: a crate can name a dependency
-- twice (normal and dev, or under two names).
CREATE TABLE IF NOT EXISTS declarations (
    dependency TEXT NOT NULL REFERENCES dependencies(name),
    crate TEXT NOT NULL REFERENCES crates(name),
    extern_name TEXT NOT NULL,
    kind TEXT NOT NULL,            -- normal, dev or build
    version_req TEXT NOT NULL,
    optional INTEGER NOT NULL,
    default_features INTEGER NOT NULL,
    features TEXT NOT NULL,        -- JSON array
    target TEXT
);
-- One row per file that refers to a dependency.
CREATE TABLE IF NOT EXISTS usages (
    dependency TEXT NOT NULL REFERENCES dependencies(name),
    crate TEXT NOT NULL REFERENCES crates(name),
    file TEXT NOT NULL,
    is_test INTEGER NOT NULL,      -- under tests/, benches/, or a test module file
    path_refs INTEGER NOT NULL,    -- lines naming `dep::...` (imports included)
    symbol_refs INTEGER NOT NULL,  -- other lines naming a symbol imported from it
    symbols TEXT NOT NULL,         -- JSON array of the paths and imported names seen
    PRIMARY KEY (dependency, file)
);
CREATE TABLE IF NOT EXISTS usage_sites (
    dependency TEXT NOT NULL REFERENCES dependencies(name),
    file TEXT NOT NULL,
    line INTEGER NOT NULL,
    text TEXT NOT NULL
);
-- What dropping a first-level dependency removes from the build graph:
-- crates no other first-level dependency (or workspace crate) still pulls in.
-- One at a time: clap weighs nothing while clap_complete still needs it, and
-- reqwest nothing while reqwest-middleware does.
CREATE TABLE IF NOT EXISTS dependency_weight (
    dependency TEXT PRIMARY KEY REFERENCES dependencies(name),
    transitive_crates INTEGER NOT NULL,     -- everything it reaches
    exclusive_crates INTEGER NOT NULL,      -- removed with it (itself included)
    exclusive_crate_names TEXT NOT NULL,    -- JSON array
    exclusive_source_lines INTEGER NOT NULL -- .rs lines in those crates' src/
);
-- Bytes of `.text` each crate contributes to a shipped release binary,
-- attributed to the first-level dependency that exclusively pulls it in.
CREATE TABLE IF NOT EXISTS binary_size (
    dependency TEXT NOT NULL REFERENCES dependencies(name),
    binary TEXT NOT NULL,
    text_bytes INTEGER NOT NULL,
    PRIMARY KEY (dependency, binary)
);
CREATE TABLE IF NOT EXISTS proposals (
    dependency TEXT PRIMARY KEY REFERENCES dependencies(name),
    review_rank INTEGER,            -- order reviewed, least used first
    alternative TEXT NOT NULL,      -- what would replace it
    size_impact TEXT NOT NULL,      -- expected change to build and binary
    estimated_own_lines INTEGER,    -- lines of our own code replacing it
    difficulty TEXT NOT NULL,       -- trivial, low, medium, high, infeasible
    maintainability TEXT NOT NULL,  -- what owning it would cost us
    recommendation TEXT NOT NULL,   -- drop, replace, keep, ...
    rationale TEXT NOT NULL,
    reviewed_at TEXT NOT NULL
);
DROP VIEW IF EXISTS usage_rank;
CREATE VIEW usage_rank AS
SELECT d.name AS dependency,
       COUNT(DISTINCT u.crate) AS crates_using,
       COUNT(u.file) AS files,
       COALESCE(SUM(u.path_refs + u.symbol_refs), 0) AS references_total,
       COALESCE(SUM(CASE WHEN u.is_test THEN 0 ELSE u.path_refs + u.symbol_refs END), 0)
           AS references_non_test,
       (SELECT COUNT(DISTINCT crate) FROM declarations x WHERE x.dependency = d.name)
           AS crates_declaring
FROM dependencies d LEFT JOIN usages u ON u.dependency = d.name
GROUP BY d.name;
"""

# Weight counts what a Linux build pulls in, not every platform's crates.
HOST = re.search(r"^host: (\S+)", subprocess.run(
    ["rustc", "-vV"], check=True, capture_output=True, text=True).stdout, re.M).group(1)

COMMENT = re.compile(r"^\s*(//|\*|/\*)")
USE_STMT = re.compile(r"\buse\s+(?:::)?([a-z_][a-z0-9_]*)\s*::\s*(.*?);", re.S)
USE_TAIL = re.compile(r"(\buse\s+(?:::)?[A-Za-z_]\w*\s*::)([^;]*)")
IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


def metadata(manifest):
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--manifest-path", manifest,
         "--filter-platform", HOST],
        check=True, capture_output=True, text=True, cwd=ROOT,
    ).stdout
    return json.loads(out)


def imported_names(tree):
    """Names a `use dep::<tree>;` brings into scope (renames honoured)."""
    names = set()
    for part in re.split(r"[{},]", tree):
        part = part.strip()
        if not part or part.endswith("*"):
            continue
        m = re.search(r"\bas\s+([A-Za-z_]\w*)$", part)
        if m:
            if m.group(1) != "_":
                names.add(m.group(1))
            continue
        last = part.split("::")[-1].strip()
        if IDENT.fullmatch(last) and last not in ("self", "super", "crate"):
            names.add(last)
        elif last == "self":
            seg = [s for s in part.split("::") if s.strip()]
            if len(seg) >= 2:
                names.add(seg[-2].strip())
    return names


def scan_file(text, externs):
    """{extern: (path_ref_lines, symbol_ref_lines, symbols, sites)}"""
    lines = text.splitlines()
    code = [("" if COMMENT.match(l) else l) for l in lines]
    joined = "\n".join(code)
    imports = defaultdict(set)
    for m in USE_STMT.finditer(joined):
        if m.group(1) in externs:
            imports[m.group(1)] |= imported_names(m.group(2))
    # Inside a `use`, only the root names a crate: `use std::{time::Duration}`
    # is not the `time` crate. Blank everything after the root, keeping lines.
    code = USE_TAIL.sub(lambda m: m.group(1) + re.sub(r"[^\n]", " ", m.group(2)), joined).split("\n")
    result = {}
    for ext in externs:
        # A module of the same name (`mod time;`) shadows the crate here.
        if re.search(r"\bmod\s+" + ext + r"\b", joined):
            continue
        path_re = re.compile(r"(?<![\w:])" + ext + r"\s*::\s*([A-Za-z_]\w*)?")
        extern_crate = re.compile(r"\bextern\s+crate\s+" + ext + r"\b")
        names = imports.get(ext, set())
        sym_re = re.compile(r"\b(" + "|".join(map(re.escape, sorted(names))) + r")\b") if names else None
        # `#[serde(...)]`, `#[async_trait]`, `#[zeroize(...)]`: helper attributes.
        attr_re = re.compile(r"#\[\s*" + ext + r"\b")
        path_lines, sym_lines, symbols, sites = 0, 0, set(), []
        for i, l in enumerate(code, 1):
            hits = list(path_re.finditer(l))
            if hits or extern_crate.search(l) or attr_re.search(l):
                path_lines += 1
                for h in hits:
                    symbols.add(ext + "::" + (h.group(1) or ""))
                if attr_re.search(l) and not hits:
                    symbols.add("#[" + ext + "]")
                sites.append((i, lines[i - 1].strip()[:300]))
            elif sym_re and sym_re.search(l):
                sym_lines += 1
                symbols.update(sym_re.findall(l))
                sites.append((i, lines[i - 1].strip()[:300]))
        if path_lines or sym_lines:
            result[ext] = (path_lines, sym_lines, sorted(symbols), sites)
    return result


def is_test_file(rel):
    parts = rel.split(os.sep)
    return any(p in ("tests", "benches") for p in parts) or rel.endswith("tests.rs")


def source_lines(manifest_path):
    src = os.path.join(os.path.dirname(manifest_path), "src")
    total = 0
    for dirpath, _, files in os.walk(src):
        for f in files:
            if f.endswith(".rs"):
                with open(os.path.join(dirpath, f), errors="replace") as fh:
                    total += sum(1 for _ in fh)
    return total


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("db", nargs="?", default=os.path.join(ROOT, "docs/dependencies/catalog.sqlite"))
    ap.add_argument("--bloat", action="append", default=[], metavar="BINARY=FILE")
    args = ap.parse_args()

    os.makedirs(os.path.dirname(args.db), exist_ok=True)
    db = sqlite3.connect(args.db)
    db.executescript(SCHEMA)
    tables = ["usage_sites", "usages", "declarations", "dependency_weight", "proposals"]
    if args.bloat:
        tables.append("binary_size")
    for t in tables + ["dependencies", "crates"]:
        db.execute(f"DELETE FROM {t}")

    workspaces = [metadata(os.path.join(ROOT, "Cargo.toml")),
                  metadata(os.path.join(ROOT, "fuzz/Cargo.toml"))]
    members, packages, resolve_nodes = {}, {}, {}
    for meta in workspaces:
        ids = set(meta["workspace_members"])
        for p in meta["packages"]:
            packages[p["id"]] = p
            if p["id"] in ids:
                members[p["name"]] = p
        # fuzz/ has its own Cargo.lock: where it resolves a shared crate's
        # dependencies to other versions, the main workspace's graph wins.
        for n in meta["resolve"]["nodes"]:
            resolve_nodes.setdefault(n["id"], n)
    member_names = set(members)

    for name, p in members.items():
        db.execute("INSERT OR REPLACE INTO crates VALUES (?, ?)",
                   (name, os.path.relpath(os.path.dirname(p["manifest_path"]), ROOT)))

    # Resolved package for each crate's dependency, by the resolve graph.
    resolved = {}
    for name, p in members.items():
        node = resolve_nodes[p["id"]]
        for d in node["deps"]:
            dp = packages[d["pkg"]]
            resolved[(name, dp["name"])] = dp

    externs_by_crate = defaultdict(dict)  # crate -> extern -> dependency
    for name, p in members.items():
        for d in p["dependencies"]:
            if d["name"] in member_names or d.get("path"):
                continue
            dep = d["name"]
            rp = resolved.get((name, dep))
            db.execute(
                "INSERT OR IGNORE INTO dependencies VALUES (?, ?, ?, ?, ?)",
                (dep, d.get("source") or "", rp and rp["version"],
                 rp and (rp.get("description") or "").strip(), rp and rp.get("repository")))
            ext = (d.get("rename") or dep).replace("-", "_")
            db.execute(
                "INSERT INTO declarations VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                (dep, name, ext, d["kind"] or "normal", d["req"], int(d["optional"]),
                 int(d["uses_default_features"]), json.dumps(d["features"]), d.get("target")))
            externs_by_crate[name][ext] = dep

    for name, p in members.items():
        crate_dir = os.path.dirname(p["manifest_path"])
        externs = externs_by_crate[name]
        if not externs:
            continue
        for dirpath, dirnames, files in os.walk(crate_dir):
            dirnames[:] = [d for d in dirnames if d not in ("target", "node_modules", ".git")]
            for f in files:
                if not f.endswith(".rs"):
                    continue
                full = os.path.join(dirpath, f)
                with open(full, errors="replace") as fh:
                    text = fh.read()
                rel = os.path.relpath(full, ROOT)
                test = is_test_file(os.path.relpath(full, crate_dir))
                for ext, (pl, sl, syms, sites) in scan_file(text, externs).items():
                    dep = externs[ext]
                    db.execute(
                        """INSERT INTO usages VALUES (?, ?, ?, ?, ?, ?, ?)
                           ON CONFLICT(dependency, file) DO UPDATE SET
                             path_refs = path_refs + excluded.path_refs,
                             symbol_refs = symbol_refs + excluded.symbol_refs""",
                        (dep, name, rel, int(test), pl, sl, json.dumps(syms)))
                    db.executemany("INSERT INTO usage_sites VALUES (?, ?, ?, ?)",
                                   [(dep, rel, n, t) for n, t in sites])

    # Transitive weight: reachability over non-dev edges from the workspace
    # crates, with and without each first-level dependency's edges. Each
    # workspace against its own Cargo.lock; only the main one's weights are
    # measured against the shipped binaries.
    lines_cache = {}
    weight_crates = {}
    weighed = set()
    for index, meta in enumerate(workspaces):
        nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
        member_ids = set(meta["workspace_members"])

        def edges(node_id):
            for d in nodes[node_id]["deps"]:
                if any(k["kind"] != "dev" for k in d["dep_kinds"]):
                    yield d["pkg"]

        def reach(starts, cut):
            """Everything reachable from `starts`, with the workspace crates'
            own edges to `cut` removed (other crates still reach it)."""
            seen, stack = set(), list(starts)
            while stack:
                n = stack.pop()
                if n in seen:
                    continue
                seen.add(n)
                stack.extend(e for e in edges(n) if not (n in member_ids and e in cut))
            return seen

        # Dev-only dependencies weigh against test builds: the workspace
        # crates' own dev edges are roots too.
        first_level = defaultdict(set)  # dependency -> package ids
        roots = set()
        for mid in member_ids:
            for d in nodes[mid]["deps"]:
                if d["pkg"] not in member_ids:
                    first_level[packages[d["pkg"]]["name"]].add(d["pkg"])
                    roots.add(d["pkg"])
        everything = reach(roots | member_ids, set())
        for dep, ids in first_level.items():
            if dep in weighed or not db.execute(
                    "SELECT 1 FROM dependencies WHERE name = ?", (dep,)).fetchone():
                continue
            weighed.add(dep)
            exclusive = everything - reach((roots - ids) | member_ids, ids)
            if index == 0:
                weight_crates[dep] = exclusive
            loc = 0
            for pid in exclusive:
                if pid not in lines_cache:
                    lines_cache[pid] = source_lines(packages[pid]["manifest_path"])
                loc += lines_cache[pid]
            db.execute("INSERT INTO dependency_weight VALUES (?, ?, ?, ?, ?)",
                       (dep, len(reach(ids, set())), len(exclusive),
                        json.dumps(sorted(packages[i]["name"] for i in exclusive)), loc))

    for spec in args.bloat:
        binary, path = spec.split("=", 1)
        with open(path) as fh:
            bloat = json.load(fh)
        sizes = {c["name"]: c["size"] for c in bloat["crates"]}
        for dep, exclusive in weight_crates.items():
            names = {packages[i]["name"].replace("-", "_") for i in exclusive}
            total = sum(sizes.get(n, 0) for n in names)
            if total:
                db.execute("INSERT INTO binary_size VALUES (?, ?, ?)", (dep, binary, total))

    proposals_file = os.path.join(os.path.dirname(args.db), "proposals.json")
    if os.path.exists(proposals_file):
        with open(proposals_file) as fh:
            proposals = {p["dependency"]: p for p in json.load(fh)}
        ranked = [r[0] for r in db.execute(
            "SELECT dependency FROM usage_rank ORDER BY references_total, files, dependency")]
        for rank, dep in enumerate(ranked, 1):
            p = proposals.pop(dep, None)
            if p:
                db.execute(
                    "INSERT INTO proposals VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    (dep, rank, p["alternative"], p["size_impact"], p["estimated_own_lines"],
                     p["difficulty"], p["maintainability"], p["recommendation"],
                     p["rationale"], p["reviewed_at"]))
        for dep in proposals:
            print(f"proposal for {dep}, which is no longer a dependency", file=sys.stderr)

    db.commit()
    n = db.execute("SELECT COUNT(*) FROM dependencies").fetchone()[0]
    f = db.execute("SELECT COUNT(*) FROM usages").fetchone()[0]
    print(f"{n} dependencies, {f} file usages -> {os.path.relpath(args.db, ROOT)}", file=sys.stderr)


if __name__ == "__main__":
    main()
