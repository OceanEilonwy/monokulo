#!/usr/bin/env python3
"""Record how long each test takes in a SQLite database.

Usage:
  test-timings.py [--db PATH] run [--suite rust|browser]...
  test-timings.py [--db PATH] load [--note TEXT] LABEL=JUNIT [LABEL=JUNIT ...]
  test-timings.py [--db PATH] report [--top N]

`run` runs the suites (all of them unless --suite picks some) and loads
their JUnit reports; `load` loads reports from a run made some other way,
e.g. CI's target/nextest/ci/junit.xml. LABEL is `rust` for a cargo-nextest
report, or `browser:<config>` for a Playwright one. `report` summarises the
latest run of each suite. The database defaults to target/test-timings.sqlite.

Suites run:
  rust     cargo nextest run --workspace --exclude xtask --profile ci
           --features engine/zmq, as CI runs it.
  browser  The offline Playwright configurations: coverage-browser (without
           instrumenting, so no coverage overhead), dashboard and
           real-binaries. Needs `npm ci` in e2e/browser and
           crates/monokulo/pos-ui.

Not run: the #[ignore]d stagenet/Tor Rust tests and the stagenet POS
Playwright suite (playwright.config.js), since they spend stagenet funds and
need public nodes. Their reports can still be loaded with `load`.

Each run is a row in `runs`; each test in it a row in `tests`:

  crate      the Cargo package, or `e2e/browser` for Playwright
  test_type  unit (a library or binary's own #[test]s), integration
             (a tests/ or examples/ target) or e2e (a tests/e2e_* target,
             or any browser test)
  binary     nextest's binary id, or the Playwright config and spec file
  test_name  the test's path within its binary, or its Playwright title
  duration_s seconds, as the reporter measured it
  status     passed, failed or skipped

Durations come from each test's own process (nextest) or worker
(Playwright); tests run side by side, so they add up to more than the run's
wall time, which `runs.wall_s` records. Needs no extra packages.
"""
import argparse
import os
import socket
import sqlite3
import subprocess
import sys
import time
import xml.etree.ElementTree as ET
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BROWSER = ROOT / 'e2e' / 'browser'
BROWSER_CONFIGS = ['coverage-browser', 'dashboard', 'real-binaries']

SCHEMA = """
CREATE TABLE IF NOT EXISTS runs (
  id INTEGER PRIMARY KEY,
  started_at TEXT NOT NULL,
  git_commit TEXT,
  git_dirty INTEGER,
  host TEXT,
  cpus INTEGER,
  suite TEXT NOT NULL,
  command TEXT,
  wall_s REAL,
  exit_code INTEGER,
  note TEXT
);
CREATE TABLE IF NOT EXISTS tests (
  run_id INTEGER NOT NULL REFERENCES runs(id),
  crate TEXT NOT NULL,
  test_type TEXT NOT NULL CHECK (test_type IN ('unit', 'integration', 'e2e')),
  binary TEXT NOT NULL,
  test_name TEXT NOT NULL,
  duration_s REAL NOT NULL,
  status TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS tests_by_run ON tests(run_id);
CREATE VIEW IF NOT EXISTS latest AS
  SELECT tests.* FROM tests
  WHERE run_id IN (SELECT max(id) FROM runs GROUP BY suite);
"""


def git(*args):
    try:
        return subprocess.run(['git', *args], cwd=ROOT, capture_output=True, text=True, check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return None


def status_of(case):
    for tag in ('failure', 'error'):
        if case.find(tag) is not None:
            return 'failed'
    if case.find('skipped') is not None:
        return 'skipped'
    return 'passed'


def rust_rows(path):
    """nextest names each testsuite after its binary id: `crate` for the
    library, `crate::bin/name`, `crate::test_target`, `crate::example/name`."""
    for suite in ET.parse(path).getroot().iter('testsuite'):
        binary = suite.get('name')
        crate, _, target = binary.partition('::')
        if not target or target.startswith('bin/'):
            kind = 'unit'
        elif target.startswith('e2e'):
            kind = 'e2e'
        else:
            kind = 'integration'
        for case in suite.iter('testcase'):
            yield crate, kind, binary, case.get('name'), float(case.get('time') or 0), status_of(case)


def browser_rows(path, config):
    for suite in ET.parse(path).getroot().iter('testsuite'):
        binary = f"{config}:{suite.get('name')}"
        for case in suite.iter('testcase'):
            yield 'e2e/browser', 'e2e', binary, case.get('name'), float(case.get('time') or 0), status_of(case)


def record(db, suite, rows, command=None, wall=None, exit_code=None, note=None):
    rows = list(rows)
    cur = db.execute(
        'INSERT INTO runs (started_at, git_commit, git_dirty, host, cpus, suite, command, wall_s, exit_code, note)'
        ' VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)',
        (datetime.now(timezone.utc).isoformat(timespec='seconds'), git('rev-parse', 'HEAD'),
         int(bool(git('status', '--porcelain'))), socket.gethostname(), os.cpu_count(),
         suite, command, wall, exit_code, note))
    db.executemany('INSERT INTO tests VALUES (?, ?, ?, ?, ?, ?, ?)', [(cur.lastrowid, *row) for row in rows])
    db.commit()
    total = sum(row[4] for row in rows)
    print(f'{suite}: {len(rows)} tests, {total:.0f}s summed' + (f', {wall:.0f}s wall' if wall else ''), file=sys.stderr)


def timed(cmd, cwd, env=None):
    print('+', ' '.join(cmd), file=sys.stderr)
    start = time.monotonic()
    code = subprocess.run(cmd, cwd=cwd, env={**os.environ, **(env or {})}).returncode
    return time.monotonic() - start, code


def target_dir():
    return Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target'))


def run_rust(db):
    cmd = ['cargo', 'nextest', 'run', '--workspace', '--locked', '--exclude', 'xtask',
           '--profile', 'ci', '--features', 'engine/zmq']
    # Build first, so the recorded wall time is the tests' alone.
    timed([*cmd, '--no-run'], ROOT)
    wall, code = timed(cmd, ROOT)
    # nextest keeps its store under the workspace's target/, whatever
    # CARGO_TARGET_DIR says.
    junit = ROOT / 'target' / 'nextest' / 'ci' / 'junit.xml'
    record(db, 'rust', rust_rows(junit), ' '.join(cmd), wall, code)


def run_browser(db):
    out = target_dir() / 'test-timings'
    out.mkdir(parents=True, exist_ok=True)
    # The fixture the coverage-browser specs start, and the binaries
    # real-binaries starts; built here so their build stays out of the times.
    timed(['cargo', 'build', '--locked', '-p', 'monokulo', '--example', 'coverage_fixture'], ROOT)
    timed(['cargo', 'build', '--locked', '-p', 'engine', '--bin', 'monokulo-engine', '-p', 'monokulo', '--bin', 'monokulo',
           '-p', 'engine-test-support', '--bin', 'fake-monerod'], ROOT)
    for config in BROWSER_CONFIGS:
        junit = out / f'browser-{config}.xml'
        junit.unlink(missing_ok=True)
        cmd = ['./node_modules/.bin/playwright', 'test', '-c', f'{config}.config.js', '--reporter=list,junit']
        wall, code = timed(cmd, BROWSER, {'PLAYWRIGHT_JUNIT_OUTPUT_FILE': str(junit)})
        if junit.exists():
            record(db, f'browser:{config}', browser_rows(junit, config), ' '.join(cmd), wall, code)
        else:
            print(f'browser:{config}: no report written (exit {code})', file=sys.stderr)


REPORTS = [
    ('Runs', """SELECT id, suite, started_at, substr(git_commit, 1, 9) AS git_commit, cpus,
                       round(wall_s) AS wall_s, exit_code FROM runs
                WHERE id IN (SELECT max(id) FROM runs GROUP BY suite) ORDER BY id""", ()),
    ('By crate and type', """SELECT crate, test_type, count(*) AS tests, round(sum(duration_s)) AS total_s,
                                    round(avg(duration_s), 2) AS mean_s, round(max(duration_s), 1) AS max_s
                             FROM latest GROUP BY crate, test_type ORDER BY total_s DESC""", ()),
    ('By binary', """SELECT binary, count(*) AS tests, round(sum(duration_s)) AS total_s, round(max(duration_s), 1) AS max_s
                     FROM latest GROUP BY binary ORDER BY total_s DESC LIMIT ?""", ('top',)),
    ('Slowest tests', """SELECT crate, test_type, test_name, round(duration_s, 1) AS duration_s, status
                         FROM latest ORDER BY duration_s DESC LIMIT ?""", ('top',)),
    ('Duration buckets', """SELECT CASE WHEN duration_s < 0.1 THEN 'a <0.1s' WHEN duration_s < 1 THEN 'b 0.1-1s'
                                        WHEN duration_s < 5 THEN 'c 1-5s' WHEN duration_s < 30 THEN 'd 5-30s'
                                        ELSE 'e >=30s' END AS bucket,
                                   count(*) AS tests, round(sum(duration_s)) AS total_s
                            FROM latest GROUP BY bucket ORDER BY bucket""", ()),
]


def print_report(db, top):
    for title, sql, params in REPORTS:
        cur = db.execute(sql, [top for _ in params])
        rows = cur.fetchall()
        headers = [d[0] for d in cur.description]
        cells = [headers, *[['' if v is None else str(v) for v in row] for row in rows]]
        widths = [max(len(row[i]) for row in cells) for i in range(len(headers))]
        print(f'\n{title}')
        for row in cells:
            print('  '.join(v.ljust(w) for v, w in zip(row, widths)).rstrip())


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--db', default=str(ROOT / 'target' / 'test-timings.sqlite'))
    sub = parser.add_subparsers(dest='action', required=True)
    run = sub.add_parser('run')
    run.add_argument('--suite', action='append', choices=['rust', 'browser'])
    load = sub.add_parser('load')
    load.add_argument('--note')
    load.add_argument('reports', nargs='+', metavar='LABEL=JUNIT')
    summary = sub.add_parser('report')
    summary.add_argument('--top', type=int, default=25)
    args = parser.parse_args()

    db = sqlite3.connect(args.db)
    db.executescript(SCHEMA)
    if args.action == 'report':
        print_report(db, args.top)
        return
    if args.action == 'run':
        suites = args.suite or ['rust', 'browser']
        if 'rust' in suites:
            run_rust(db)
        if 'browser' in suites:
            run_browser(db)
    else:
        for report in args.reports:
            label, _, path = report.partition('=')
            if label == 'rust':
                rows = rust_rows(path)
            elif label.startswith('browser:'):
                rows = browser_rows(path, label.removeprefix('browser:'))
            else:
                parser.error(f'unknown label {label!r}: use rust or browser:<config>')
            record(db, label, rows, note=args.note)
    print(f'wrote {args.db}', file=sys.stderr)


if __name__ == '__main__':
    main()
