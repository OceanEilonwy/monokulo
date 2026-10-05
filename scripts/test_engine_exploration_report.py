#!/usr/bin/env python3
"""Verify evidence extraction does not invent exploration or corpus growth."""
import importlib.util
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
            (p / 'semantics.1.jsonl').write_text('{"command:Mine(2)":1,"fixture:scanner-valid-synthetic":1}\n')
            (p / 'semantics.2.jsonl').write_text('{"command:Mine(8)":2}\n')
            (p / 'calibration-semantics.3.jsonl').write_text('{"command:Mine(0)":999}\n')
            cases, counts = report.semantics(p)
            self.assertEqual(cases, 2)
            self.assertEqual(counts['command:Mine'], 3)


if __name__ == '__main__': unittest.main()
