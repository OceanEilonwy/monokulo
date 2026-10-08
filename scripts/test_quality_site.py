#!/usr/bin/env python3
"""Check the quality report reads CI artifacts as they are laid out, and shows only what they hold."""
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location('site', Path(__file__).with_name('quality-site.py'))
site = importlib.util.module_from_spec(spec)
spec.loader.exec_module(site)

try:
    from PIL import Image
except ImportError:
    Image = None

SHA = '7e8f34dac14b985bd24a323147b71a10e2bc4b05'


def write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text if isinstance(text, str) else json.dumps(text))


def junit(cases):
    rows = ''.join(f'<testcase name="{n}" classname="{c}" time="{t}">{extra}</testcase>' for c, n, t, extra in cases)
    return f'<testsuites><testsuite>{rows}</testsuite></testsuites>'


def coverage_artifact(root):
    """A cut-down coverage-<sha> artifact: two crates, three suites, the stress run, one screenshot."""
    totals = lambda l, lt, b, bt: {'lines': {'covered': l, 'total': lt}, 'branches': {'covered': b, 'total': bt}}
    write(root / 'run.json', {'revision': SHA, 'components': [{'component': 'rust', 'status': 'passed'}, {'component': 'browser', 'status': 'passed'}]})
    write(root / 'rust.json', {**totals(90, 100, 8, 10), 'tools': {'collector': 'cargo-llvm-cov 0.9.1'}})
    write(root / 'browser.json', {**totals(19, 20, 9, 10), 'tools': {'collector': 'istanbul'}})
    write(root / 'rust-crates.json', [
        {'component': 'engine', 'report': 'rust/crates/engine.html', 'unavailable_files': ['crates/engine/src/lib.rs'], **totals(80, 90, 8, 9)},
        {'component': 'cli-wallet', 'report': 'rust/crates/cli-wallet.html', 'unavailable_files': [], **totals(10, 40, 0, 1)},
    ])
    write(root / 'rust/crates/engine.html', '<table><tr><th>Source</th></tr><tr><td><a href="../coverage/home/runner/crates/engine/src/scanner.rs.html">src/scanner.rs</a></td><td>80/90</td><td>8/9</td></tr></table>')
    write(root / 'rust/crates/cli-wallet.html', '<table></table>')
    write(root / 'rust/index.html', '<html></html>')
    write(root / 'rust/coverage/home/runner/crates/engine/src/scanner.rs.html', '<html></html>')
    write(root / 'rust/junit.xml', junit([('engine', 'scanner::tests::a_reorg_moves_the_payment', 0.5, ''),
                                          ('engine', 'scanner::tests::a_broken_case', 0.1, '<failure/>'),
                                          ('engine', 'scanner::tests::an_ignored_case', 0, '<skipped/>')]))
    write(root / 'browser/junit-fixture.xml', junit([('checkout.spec.js', 'checkout shows the amount', 2.0, '')]))
    write(root / 'browser/index.html', '<html></html>')
    point = lambda tenants, ms: {'tenants': tenants, 'status': 'sustainable', 'peak_resident_bytes': 24 * 1048576, 'fixture': {
        'tenants': tenants, 'http_max_latency_us': 5000, 'measured_ticks': 2, 'drain_ticks': 1,
        'points': [{'phase': 'warmup', 'duration_ms': 90, 'lagging_tenants': 0, 'oldest_lag_blocks': 0},
                   {'phase': 'measured', 'duration_ms': ms, 'lagging_tenants': 0, 'oldest_lag_blocks': 0},
                   {'phase': 'measured', 'duration_ms': ms, 'lagging_tenants': 0, 'oldest_lag_blocks': 0}]}}
    fault = {'file': 'fault-rpc', 'status': 'recovered', 'tenants': 16, 'fixture': {
        'rpc_calls': 70, 'rpc_failures': 6, 'rpc_delay_ms': 2, 'rpc_fail_every': 3, 'rpc_fail_until_height': 2, 'measured_ticks': 2, 'drain_ticks': 6,
        'points': [{'phase': 'measured', 'duration_ms': 70, 'lagging_tenants': 0, 'oldest_lag_blocks': 0,
                    'process_memory': {'peak_resident_bytes': 1048576}}]}}
    write(root / 'stress/run.json', {'profile': 'ci', 'hardware': 'hardware.json', 'scenario': {'poll_interval_ms': 5000, 'max_oldest_lag_blocks': 3, 'max_http_latency_ms': 250},
                                     'results': [point(32, 58), point(128, 107)], 'faults': [fault]})
    write(root / 'stress/hardware.json', {'cpu_model': 'Test CPU', 'effective_cores': 4, 'sqlite_version': '3.53.2', 'selected_cpu': 0})
    write(root / 'stress/index.html', '<html></html>')
    if Image:
        shots = root / 'screenshots'
        (shots / 'images').mkdir(parents=True)
        Image.new('RGB', (1280, 800), 'white').save(shots / 'images/checkout-paid.png')
        Image.new('RGB', (390, 844), 'white').save(shots / 'images/checkout-paid-phone.png')
        common = {'group': 'checkout', 'stage': 'checkout-paid', 'test': 'checkout shows paid', 'status': 'passed', 'retry': 0, 'report': '../browser/playwright-report/index.html'}
        write(shots / 'manifest.json', [
            {**common, 'shape': 'desktop', 'theme': 'light', 'image': 'images/checkout-paid.png'},
            {**common, 'shape': 'mobile-portrait', 'theme': 'light', 'image': 'images/checkout-paid-phone.png'},
            {**common, 'shape': 'desktop', 'theme': 'light', 'image': 'images/checkout-paid.png', 'retry': 1},
        ])


def exploration_artifacts(root):
    """Property and fuzz reports for both engine builds, as their artifacts lay them out."""
    for feature, cases in (('zmq', 1114), ('', 900)):
        build = feature or 'default'
        write(root / f'properties-{build}/target/engine-exploration/properties/report.json',
              {'revision': SHA, 'settings': {'PROPTEST_CASES': '512', 'ENGINE_FEATURES': feature}, 'semantic_cases': cases,
               'semantic_observations': {'sql-denial-reached': 12515}})
        write(root / f'properties-{build}/target/nextest/ci/junit.xml', junit([('engine', 'work::tests::properties::money_matches_the_model', 900, '')]))
        for target, edges in (('portfolio', 28476), ('inputs', 6355)):
            write(root / f'fuzz/engine-fuzz-{target}-{build}-1/target/engine-exploration/fuzz/{target}/{build}/1/abc/report.json',
                  {'status': 'passed', 'wall_seconds': 400.4, 'corpus': {'files': 9}, 'new_unique_inputs': 3,
                   'exploration': {'final': {'coverage': edges + (0 if feature else 1), 'executions': 724}, 'coverage_growth': 35},
                   'semantic_cases': 2, 'semantic_observations': {}})


class QualitySiteTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        coverage_artifact(self.root / 'coverage')
        exploration_artifacts(self.root / 'runs')
        self.out = self.root / 'site/quality'

    def tearDown(self):
        self.tmp.cleanup()

    def build(self, *args):
        site.main(['--out', str(self.out), *map(str, args)])
        return json.loads((self.out / 'data.json').read_text())

    def test_coverage_links_each_file_to_its_annotated_source(self):
        data = self.build('--coverage', self.root / 'coverage')
        engine = next(c for c in data['coverage']['crates'] if c['name'] == 'engine')
        name, covered, total, _, _, link = engine['files'][0]
        self.assertEqual((name, covered, total), ('src/scanner.rs', 80, 90))
        self.assertEqual(link, 'reports/rust/coverage/home/runner/crates/engine/src/scanner.rs.html')
        self.assertTrue((self.out / link).is_file())
        self.assertEqual(engine['unmeasured'], ['crates/engine/src/lib.rs'])

    def test_test_tools_stay_out_of_the_shipping_figure_and_the_badge(self):
        data = self.build('--coverage', self.root / 'coverage')
        self.assertEqual(data['coverage']['shipping'], [80, 90, 8, 9])
        self.assertTrue(next(c for c in data['coverage']['crates'] if c['name'] == 'cli-wallet')['tool'])
        self.assertEqual(json.loads((self.out / 'badge.json').read_text())['message'], '88.9%')

    def test_every_test_keeps_its_result(self):
        data = self.build('--coverage', self.root / 'coverage')
        statuses = [row[3] for row in data['coverage']['tests']['rust']]
        self.assertEqual(statuses, ['passed', 'failed', 'skipped'])
        self.assertEqual(data['coverage']['tests']['browser'][0][4], 'fixture')

    def test_stress_run_reads_its_hardware_file_and_fault_settings(self):
        stress = self.build('--coverage', self.root / 'coverage')['stress']
        self.assertEqual(stress['budget_ms'], 5000)
        self.assertEqual(stress['hardware']['cpu_model'], 'Test CPU')
        self.assertEqual([p['tenants'] for p in stress['points']], [32, 128])
        self.assertEqual(stress['points'][1]['ticks'][1], ['measured', 107, 0, 0])
        rpc = stress['faults'][0]
        self.assertEqual((rpc['name'], rpc['rpc_fail_every'], rpc['rpc_failures'], rpc['peak_mb']), ('fault-rpc', 3, 6, 1.0))

    def test_only_the_named_build_of_the_nightly_runs_is_shown(self):
        data = self.build('--properties', self.root / 'runs', '--fuzz', self.root / 'runs')
        self.assertEqual(data['properties']['cases'], 1114)
        self.assertEqual([f['target'] for f in data['fuzz']], ['portfolio', 'inputs'])
        self.assertEqual(data['fuzz'][0]['edges'], 28476)
        default = self.build('--properties', self.root / 'runs', '--feature', 'default')
        self.assertEqual(default['properties']['cases'], 900)

    @unittest.skipUnless(Image, 'Pillow is not installed')
    def test_gallery_keeps_a_thumbnail_and_the_full_image_of_each_first_try(self):
        gallery = self.build('--coverage', self.root / 'coverage')['gallery']
        self.assertEqual(gallery['total'], 2)
        stage = gallery['stages'][0]
        self.assertEqual(stage['shapes'], ['desktop', 'mobile-portrait'])
        desktop = stage['images']['desktop']['light']
        self.assertEqual((desktop['w'], desktop['fw']), (480, 1280))
        self.assertEqual(stage['images']['mobile-portrait']['light']['w'], 280)
        self.assertTrue((self.out / desktop['full']).is_file())
        self.assertEqual(stage['report'], 'reports/browser/playwright-report/index.html')

    def test_missing_inputs_leave_their_sections_out_but_still_build_the_page(self):
        data = self.build()
        for key in ('coverage', 'properties', 'fuzz', 'scale'):
            self.assertIsNone(data.get(key))
        self.assertEqual(json.loads((self.out / 'badge.json').read_text())['message'], 'unknown')
        page = (self.out / 'index.html').read_text()
        self.assertNotIn('@REPO_URL@', page)
        self.assertTrue((self.out / 'assets/theme.css').is_file())

    def test_a_rebuild_starts_clean(self):
        self.build('--coverage', self.root / 'coverage')
        self.build()
        self.assertFalse((self.out / 'reports').exists())


if __name__ == '__main__':
    unittest.main()
