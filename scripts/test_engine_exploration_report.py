#!/usr/bin/env python3
"""Verify evidence extraction does not invent exploration or corpus growth."""
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('report', Path(__file__).with_name('engine-exploration-report.py'))
report = importlib.util.module_from_spec(spec)
spec.loader.exec_module(report)


class EvidenceTests(unittest.TestCase):
    def test_complete_libfuzzer_campaign(self):
        result = report.summarize('#8 INITED cov: 10 ft: 12 corp: 3/10b\n#42 NEW cov: 13 ft: 19\n#55 DONE cov: 13 ft: 20\nstat::number_of_executed_units: 55\nstat::average_exec_per_sec: 5\n')
        self.assertEqual(result['coverage_growth'], 3)
        self.assertEqual(result['executions_after_initialization'], 47)
        self.assertEqual(result['feature_growth'], 8)
        self.assertEqual(result['stats']['number_of_executed_units'], 55)

    def test_no_coverage_claim_for_uninstrumented_seed_replay(self):
        result = report.summarize('Running seed once\nDone\n')
        self.assertIsNone(result['coverage_growth'])
        self.assertIsNone(result['final'])

    def test_failure_before_initialization_has_no_growth(self):
        result = report.summarize('#12 NEW cov: 9 ft: 10\nERROR: AddressSanitizer\n')
        self.assertIsNone(result['coverage_growth'])
        self.assertEqual(result['final']['executions'], 12)

    def test_corpus_growth_counts_unique_contents(self):
        with tempfile.TemporaryDirectory() as directory:
            p = Path(directory)
            (p / 'one').write_bytes(b'same'); (p / 'two').write_bytes(b'same')
            (p / 'three').write_bytes(b'new'); (p / 'nested').mkdir()
            result = report.corpus(p)
            self.assertEqual(result['files'], 3)
            self.assertEqual(result['bytes'], 11)
            self.assertEqual(len(result['sha256']), 2)

    def test_semantic_counts_aggregate_families_without_calibration(self):
        with tempfile.TemporaryDirectory() as directory:
            p = Path(directory)
            (p / 'semantics.1.jsonl').write_text('{"selected-command:Mine(2)":1,"applied-transition:Mine":1,"fixture:scanner-valid-synthetic":1}\n')
            (p / 'semantics.2.jsonl').write_text('{"selected-command:Mine(8)":2,"skipped-command:Mine":2}\n')
            (p / 'calibration-semantics.3.jsonl').write_text('{"command:Mine(0)":999}\n')
            cases, counts = report.semantics(p)
            self.assertEqual(cases, 2)
            self.assertEqual(counts['selected-command:Mine'], 3)
            self.assertEqual(counts['applied-transition:Mine'], 1)
            self.assertEqual(counts['skipped-command:Mine'], 2)




class CampaignAcceptanceTests(unittest.TestCase):
    def run_cli(self, *args):
        return subprocess.run([sys.executable, str(Path(report.__file__).resolve()), *map(str, args)],
                              text=True, capture_output=True)

    def test_finish_rejects_missing_malformed_negative_zero_and_incomplete_evidence(self):
        logs = [
            ('Running seed once\nDone\n', 'invalid-evidence'),
            ('#8 INITED cov: unknown ft: 12\n#55 DONE cov: 13 ft: 20\n', 'invalid-evidence'),
            ('#8 INITED cov: 10 ft: 12\n#7 DONE cov: 10 ft: 12\n', 'invalid-evidence'),
            ('#8 INITED cov: 10 ft: 12\n#8 DONE cov: 10 ft: 12\n', 'insufficient-exploration'),
            ('#8 INITED cov: 10 ft: 12\n#9 NEW cov: 11 ft: 13\n', 'invalid-evidence'),
            ('#8 INITED cov: 10 ft: 12\n#9 DONE cov: 11 ft: 13\n', 'passed'),
        ]
        for log, expected in logs:
            with self.subTest(expected=expected, log=log), tempfile.TemporaryDirectory() as directory:
                out = Path(directory)
                (out / 'replay.json').write_text(json.dumps({'started': time.time(), 'corpus': {'sha256': []}}))
                (out / 'fuzzer.log').write_text(log)
                result = self.run_cli('finish', '--output', out, '--corpus', out / 'corpus', '--exit-code', 0)
                self.assertEqual(result.returncode == 0, expected == 'passed', result.stderr)
                self.assertEqual(json.loads((out / 'report.json').read_text())['status'], expected)

    def test_empty_and_missing_calibration_fail_explicitly(self):
        for exists in [False, True]:
            with self.subTest(exists=exists), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                seeds = root / 'seeds'
                if exists:
                    seeds.mkdir()
                result = self.run_cli('calibrate', '--output', root / 'report', '--seeds', seeds,
                                      '--binary', root / 'unused-binary', '--timeout', 1)
                self.assertNotEqual(result.returncode, 0)
                data = json.loads((root / 'report/calibration.json').read_text())
                self.assertEqual(data['status'], 'failed')
                self.assertEqual(data['seeds'], [])

    def test_begin_refuses_previous_evidence_without_changing_it(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            previous = root / 'report.json'
            previous.write_text('{"status":"passed"}')
            result = self.run_cli('begin', '--output', root, '--corpus', root / 'corpus',
                                  '--seeds', root / 'seeds', '--target', 'status', '--seconds', 1,
                                  '--timeout', 1, '--max-len', 10, '--seed', 1)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(previous.read_text(), '{"status":"passed"}')

    def test_sync_seeds_keeps_discoveries_and_refreshes_changed_reviewed_contents(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            seeds, corpus = root / 'seeds', root / 'corpus'
            seeds.mkdir()
            corpus.mkdir()
            (seeds / 'named').write_bytes(b'updated')
            (corpus / 'named').write_bytes(b'cached old input')
            (corpus / 'discovery').write_bytes(b'discovered')
            report.sync_seeds(seeds, corpus)
            reviewed = next(corpus.glob('reviewed-*'))
            self.assertEqual(reviewed.read_bytes(), b'updated')
            reviewed.write_bytes(b'changed by an earlier campaign')
            report.sync_seeds(seeds, corpus)
            self.assertEqual(reviewed.read_bytes(), b'updated')
            self.assertEqual((corpus / 'discovery').read_bytes(), b'discovered')
            self.assertEqual((seeds / 'named').read_bytes(), b'updated')


class ShellRunnerTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        scripts = self.root / 'scripts'
        scripts.mkdir()
        for name in ['engine-fuzz.sh', 'engine-exploration-report.py']:
            shutil.copyfile(Path(__file__).with_name(name), scripts / name)
        (self.root / 'Cargo.lock').write_text('root lock')
        seeds = self.root / 'fuzz/seeds/status'
        seeds.mkdir(parents=True)
        (seeds / 'boundary').write_bytes(b'reviewed')
        (self.root / 'fuzz/Cargo.lock').write_text('fuzz lock')
        binaries = self.root / 'bin'
        binaries.mkdir()
        stubs = {
            'git': '#!/bin/sh\ncase "$1" in rev-parse) echo abcdef0123456789;; status) ;; esac\n',
            'rustc': '#!/bin/sh\necho "rustc test compiler"\necho "host: x86_64-unknown-linux-gnu"\n',
            'cargo': r'''#!/usr/bin/env python3
import os
from pathlib import Path
import sys
args = sys.argv[1:]
failure = os.environ.get('ENGINE_TEST_FAILURE')
if args[:2] in [['fuzz', 'build'], ['fuzz', 'run']] and os.environ.get('ENGINE_TEST_MUSL_DEFAULT'):
    if '--target' not in args or args[args.index('--target') + 1] != 'x86_64-unknown-linux-gnu':
        print('sanitizer is incompatible with statically linked libc', file=sys.stderr)
        sys.exit(42)
if args[0] == 'metadata':
    sys.exit(17 if failure == 'metadata' else 0)
if args[:2] == ['fuzz', 'build']:
    if failure == 'build': sys.exit(19)
    binary = Path('fuzz/target/x86_64-unknown-linux-gnu/release/status')
    binary.parent.mkdir(parents=True, exist_ok=True)
    binary.write_text('#!/bin/sh\nif [ "$ENGINE_TEST_FAILURE" = calibration ]; then exit 23; fi\nexit 0\n')
    binary.chmod(0o755)
elif args[:2] == ['fuzz', 'run']:
    print('#8 INITED cov: 10 ft: 12')
    print('#55 DONE cov: 13 ft: 20')
    sys.exit(41 if failure == 'exploration' else 0)
else:
    print('test tool version')
''',
        }
        for name, body in stubs.items():
            path = binaries / name
            path.write_text(body)
            path.chmod(0o755)
        self.env = dict(os.environ, PATH=str(binaries) + os.pathsep + os.environ['PATH'],
                        RUSTUP_TOOLCHAIN='fixture', ENGINE_FUZZ_SEED='1', PYTHONDONTWRITEBYTECODE='1')

    def run_campaign(self, failure=None):
        env = dict(self.env)
        if failure:
            env['ENGINE_TEST_FAILURE'] = failure
        return subprocess.run(['bash', 'scripts/engine-fuzz.sh', 'status', '1'], cwd=self.root,
                              env=env, text=True, capture_output=True)

    def reports(self):
        return list((self.root / 'target/engine-exploration/fuzz/status/default/1').glob('*/report.json'))

    def test_same_seed_runs_have_independent_evidence_even_when_second_build_fails(self):
        first = self.run_campaign()
        self.assertEqual(first.returncode, 0, first.stderr + first.stdout)
        original = self.reports()[0]
        content = original.read_bytes()
        second = self.run_campaign('build')
        self.assertEqual(second.returncode, 19, second.stderr)
        self.assertEqual(len(self.reports()), 2)
        self.assertEqual(original.read_bytes(), content)
        other = next(p for p in self.reports() if p != original)
        self.assertEqual(json.loads(other.read_text())['stage'], 'build')
        self.assertEqual(json.loads(other.read_text())['status'], 'failed')

    def test_prebuilt_musl_fuzzer_uses_rustc_host_for_build_and_run(self):
        self.env['ENGINE_TEST_MUSL_DEFAULT'] = '1'
        result = self.run_campaign()
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        data = json.loads(self.reports()[0].read_text())
        self.assertEqual(data['status'], 'passed')
        self.assertEqual(data['exploration']['executions_after_initialization'], 47)

    def test_metadata_calibration_and_exploration_failures_are_terminal(self):
        for stage, code in [('metadata', 17), ('calibration', 1), ('exploration', 41)]:
            with self.subTest(stage=stage):
                result = self.run_campaign(stage)
                self.assertEqual(result.returncode, code, result.stderr + result.stdout)
        data = [json.loads(p.read_text()) for p in self.reports()]
        self.assertEqual(len(data), 3)
        self.assertTrue(all(d['status'] == 'failed' for d in data))
        self.assertIn('calibration', [d.get('stage') for d in data])
        exploration = next(d for d in data if d['exit_code'] == 41)
        self.assertEqual(exploration['exploration']['executions_after_initialization'], 47)
        self.assertNotIn('stage', exploration)


if __name__ == '__main__':
    unittest.main()
