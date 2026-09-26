#!/usr/bin/env python3
"""Print the generated component manifests as a GitHub job-summary table."""
import json
from pathlib import Path

root = Path('target/coverage')
run = root / 'run.json'
print('## Coverage')
print()
print('| Component | Tests | Lines | Branches | Report |')
print('| --- | --- | ---: | ---: | --- |')
if not run.is_file():
    print('| all | unavailable | unavailable | unavailable | [artifact](.) |')
else:
    for item in json.loads(run.read_text()).get('components', []):
        name = item['component']
        manifest = root / f'{name}.json'
        data = json.loads(manifest.read_text()) if manifest.is_file() else {}
        def metric(key):
            value = data.get(key, {})
            return f"{value['covered']}/{value['total']}" if value.get('total') is not None else 'unavailable'
        report = data.get('report')
        link = f'`{report}`' if report and (root / report).is_file() else 'unavailable'
        print(f"| {name} | {item['status']} | {metric('lines')} | {metric('branches')} | {link} |")
