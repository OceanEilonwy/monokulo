#!/usr/bin/env python3
"""Validate the portable all-profile coverage artifact without extra packages."""
import json
import sys
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote

repo = Path(__file__).resolve().parent.parent
root = repo / 'target/coverage'
schema = json.loads((repo / 'docs/coverage-manifest.schema.json').read_text())
baseline = json.loads((repo / 'docs/coverage-line-baseline.json').read_text())


def check(value, rule, where):
    if '$ref' in rule:
        return check(value, schema['$defs'][rule['$ref'].split('/')[-1]], where)
    kind = rule.get('type')
    if kind is not None:
        kinds = kind if isinstance(kind, list) else [kind]
        actual = ('null' if value is None else 'boolean' if isinstance(value, bool)
                  else 'integer' if isinstance(value, int) else 'string' if isinstance(value, str)
                  else 'array' if isinstance(value, list) else 'object' if isinstance(value, dict) else 'other')
        assert actual in kinds, f'{where}: expected {kinds}, got {actual}'
    if 'enum' in rule:
        assert value in rule['enum'], f'{where}: invalid value {value!r}'
    if isinstance(value, str):
        assert len(value) >= rule.get('minLength', 0), f'{where}: empty string'
    if isinstance(value, int):
        assert value >= rule.get('minimum', -sys.maxsize), f'{where}: below minimum'
    if isinstance(value, dict):
        for name in rule.get('required', []):
            assert name in value, f'{where}: missing {name}'
        properties = rule.get('properties', {})
        extra = rule.get('additionalProperties', True)
        for name, item in value.items():
            child = properties.get(name, extra)
            assert child is not False, f'{where}: unexpected {name}'
            if isinstance(child, dict):
                check(item, child, f'{where}.{name}')
    if isinstance(value, list):
        if rule.get('uniqueItems'):
            assert len({json.dumps(item, sort_keys=True) for item in value}) == len(value), f'{where}: duplicates'
        if 'items' in rule:
            for index, item in enumerate(value):
                check(item, rule['items'], f'{where}[{index}]')


def local_file(relative):
    assert isinstance(relative, str) and relative and not Path(relative).is_absolute(), f'invalid report path {relative}'
    assert '..' not in Path(relative).parts, f'path escapes artifact: {relative}'
    path = root / relative
    assert path.is_file(), f'missing artifact: {relative}'
    return path


class Links(HTMLParser):
    def __init__(self):
        super().__init__()
        self.refs = []

    def handle_starttag(self, tag, attrs):
        attributes = dict(attrs)
        ref = attributes.get('href') or attributes.get('src')
        if tag in ('a', 'img', 'script', 'link') and ref:
            self.refs.append(ref)


def check_html(path):
    links = Links()
    links.feed(path.read_text())
    for ref in links.refs:
        if ref.startswith(('http:', 'https:', 'data:', '#', 'mailto:', 'javascript:')):
            continue
        target = unquote(ref.split('#')[0].split('?')[0])
        if target:
            assert (path.parent / target).is_file(), f'broken link in {path}: {ref}'


def main():
    check(json.loads((repo / 'docs/coverage-manifest.example.json').read_text()), schema, 'fixture')
    run = json.loads(local_file('run.json').read_text())
    components = run['components']
    assert [item['component'] for item in components] == ['rust', 'browser', 'woocommerce'], 'all run needs three ordered components'
    for item in components:
        name = item['component']
        assert item['status'] == 'passed' and item['exit_code'] == 0, f'{name}: tests failed'
        local_file(item['log'])
        data = json.loads(local_file(f'{name}.json').read_text())
        check(data, schema, name)
        assert data['revision'] == run['revision'], f'{name}: different source revision'
        assert data['test']['status'] == 'passed' and not data['unavailable'], f'{name}: incomplete metrics'
        for metric in ('lines', 'branches'):
            counts = data[metric]
            assert isinstance(counts['total'], int) and isinstance(counts['covered'], int) \
                and counts['total'] > 0 and 0 <= counts['covered'] <= counts['total'], f'{name}: invalid {metric}'
        check_html(local_file(data['report']))
        prior = baseline['components'][name]
        if data['tools'] == prior['tools']:
            assert data['lines']['covered'] >= prior['floor_lines'], f'{name}: below reviewed line floor'
        else:
            print(f'{name}: tool versions changed; line floor is trend-only until reviewed')
    check_html(local_file('index.html'))
    for area in ('rust', 'browser', 'woocommerce', 'screenshots'):
        for page in (root / area).rglob('*.html'):
            check_html(page)
    entries = json.loads(local_file('screenshots/manifest.json').read_text())
    assert len(entries) >= 10, 'fewer than ten screenshots'
    assert {'checkout', 'pos', 'challenge'} <= {e['group'] for e in entries}, 'missing screenshot group'
    images = set()
    for entry in entries:
        image = entry['image']
        assert image.startswith('images/') and image not in images, f'invalid or duplicate screenshot {image}'
        images.add(image)
        assert local_file('screenshots/' + image).stat().st_size > 0, f'empty screenshot {image}'
    php = json.loads(local_file('woocommerce/summary.json').read_text())
    assert {f['name'] for f in php['files']} == {'monokulo.php', 'class-wc-gateway-monokulo.php'}, 'unexpected PHP source'
    browser = json.loads(local_file('browser/coverage-final.json').read_text())
    expected = {'checkout.js', 'challenge.js', 'monokulo-client.js', 'main.tsx'}
    assert {Path(name).name for name in browser} == expected, 'unexpected browser source'
    print('coverage artifact validation passed')


if __name__ == '__main__':
    try:
        main()
    except (AssertionError, KeyError, OSError, ValueError) as error:
        print(f'coverage artifact validation failed: {error}', file=sys.stderr)
        sys.exit(1)
