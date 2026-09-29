#!/usr/bin/env python3
"""Print JUnit reports as a GitHub job-summary table and a list of failures.

Usage: test-summary.py TITLE LABEL=PATH [LABEL=PATH ...]

Each LABEL=PATH is one row: a suite's JUnit file (cargo-nextest, Playwright
and PHPUnit all write one). A missing file is a row that says so, since a
suite that never reported is not a pass. Needs no extra packages.
"""
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

MESSAGE_LIMIT = 3000


def read(path):
    """Every testcase in the file, however its testsuites nest, and the run's
    wall time: the root's own time where the tool records one (tests run in
    parallel, so their times add up to more), else the sum."""
    root = ET.parse(path).getroot()
    found = list(root.iter('testcase'))
    seconds = root.get('time')
    if seconds is None and root.tag == 'testsuites':
        suites = root.findall('testsuite')
        seconds = suites[0].get('time') if len(suites) == 1 else None
    if seconds is None:
        seconds = sum(float(case.get('time') or 0) for case in found)
    return found, float(seconds)


def outcome(case):
    for tag in ('failure', 'error'):
        found = case.find(tag)
        if found is not None:
            return 'failed', found
    if case.find('skipped') is not None:
        return 'skipped', None
    # cargo-nextest's JUnit keeps a retried test's failed attempts.
    for tag in ('flakyFailure', 'flakyError'):
        found = case.find(tag)
        if found is not None:
            return 'flaky', found
    return 'passed', None


def message(element):
    text = (element.get('message') or '').strip()
    body = (element.text or '').strip()
    if body and body not in text:
        text = f'{text}\n{body}' if text else body
    if len(text) > MESSAGE_LIMIT:
        text = text[:MESSAGE_LIMIT] + '\n…'
    return text


def main():
    if len(sys.argv) < 3:
        print(__doc__, file=sys.stderr)
        return 2
    title, rows = sys.argv[1], []
    failures, flaky = [], []
    for argument in sys.argv[2:]:
        label, _, path = argument.partition('=')
        path = Path(path)
        if not path.is_file():
            rows.append((label, None))
            continue
        counts = {'passed': 0, 'failed': 0, 'flaky': 0, 'skipped': 0}
        try:
            found, seconds = read(path)
            for case in found:
                state, detail = outcome(case)
                counts[state] += 1
                if detail is not None:
                    name = ' › '.join(part for part in (case.get('classname'), case.get('name')) if part)
                    (flaky if state == 'flaky' else failures).append((label, name, message(detail)))
        except ET.ParseError as error:
            rows.append((label, f'unreadable report: {error}'))
            continue
        rows.append((label, (counts, seconds)))

    print(f'## {title}')
    print()
    print('| Suite | Result | Passed | Failed | Flaky | Skipped | Time |')
    print('| --- | --- | ---: | ---: | ---: | ---: | ---: |')
    total = {'passed': 0, 'failed': 0, 'flaky': 0, 'skipped': 0}
    for label, data in rows:
        if not isinstance(data, tuple):
            print(f'| {label} | ⚠️ {data or "no report"} | | | | | |')
            continue
        counts, seconds = data
        for key in total:
            total[key] += counts[key]
        result = '❌ failed' if counts['failed'] else '⚠️ passed on retry' if counts['flaky'] else '✅ passed'
        print(f"| {label} | {result} | {counts['passed']} | {counts['failed']} | {counts['flaky']} | {counts['skipped']} | {seconds:.0f}s |")
    print(f"| **Total** | | **{total['passed']}** | **{total['failed']}** | **{total['flaky']}** | **{total['skipped']}** | |")
    print()
    for heading, listed in (('Failures', failures), ('Flaky: failed, then passed on retry', flaky)):
        if not listed:
            continue
        print(f'### {heading} ({len(listed)})')
        print()
        for label, name, text in listed:
            first = text.splitlines()[0] if text else 'no message'
            print(f'<details><summary><b>{label}</b>: <code>{escape(name)}</code>: {escape(first[:200])}</summary>')
            print()
            print('```')
            print(text.replace('```', '`​``'))
            print('```')
            print('</details>')
            print()
    return 0


def escape(text):
    return text.replace('&', '&amp;').replace('<', '&lt;').replace('>', '&gt;')


if __name__ == '__main__':
    sys.exit(main())
