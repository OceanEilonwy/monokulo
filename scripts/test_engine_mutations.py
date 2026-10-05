#!/usr/bin/env python3
"""Check mutation-runner outcomes using actual tiny Cargo test programs."""
import importlib.util
import json
import os
from pathlib import Path
import sys
import subprocess
import time
import signal
import tempfile
import unittest

sys.dont_write_bytecode = True

SPEC = importlib.util.spec_from_file_location("engine_mutations", Path(__file__).with_name("engine-mutations.py"))
RUNNER = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = RUNNER
SPEC.loader.exec_module(RUNNER)


class OutcomeTests(unittest.TestCase):
    def exercise(self, source, timeout=30, expected_failure=None, required_hits=()):
        with tempfile.TemporaryDirectory(prefix="mutation-runner-outcome-") as directory:
            root = Path(directory)
            (root / "src").mkdir()
            (root / "Cargo.toml").write_text('[package]\nname="outcome_fixture"\nversion="0.0.0"\nedition="2024"\n[workspace]\n')
            (root / "src/lib.rs").write_text(source)
            env = os.environ.copy()
            env.update(CARGO_TERM_COLOR="never", CARGO_TARGET_DIR=str(root / "build"),
                       RUNNER_TEST_PID_PATH=str(root / "child.pid"))
            result = RUNNER.run(["cargo", "test", "--offline", "--lib", "--", "--nocapture"],
                                root, env, root / "result.log", timeout, expected_failure, required_hits)
            marker = root / "child.pid"
            if marker.exists() and os.name == "posix":
                pid = int(marker.read_text())
                result["fixture_pid"] = pid
                deadline = time.monotonic() + 2
                while True:
                    status = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)],
                                            capture_output=True, text=True, check=False).stdout.strip()
                    alive = bool(status) and not status.startswith("Z")
                    if not alive or time.monotonic() >= deadline:
                        result["child_still_running"] = alive
                        if alive:
                            os.kill(pid, signal.SIGKILL)
                        break
                    time.sleep(0.02)
            return result

    def test_healthy_assertion_is_a_baseline(self):
        result = self.exercise("#[test] fn healthy(){assert_eq!(1,1);}")
        self.assertTrue(result["passed"])
        self.assertFalse(result["detected"])

    def test_assertion_failure_detects_a_defect(self):
        result = self.exercise("#[test] fn defect(){assert_eq!(1,2);}")
        self.assertTrue(result["detected"])
        self.assertFalse(result["passed"])

    def test_intended_assertion_detects_the_named_defect(self):
        result = self.exercise('#[test] fn defect(){assert_eq!(1,2,"BOUNDARY: money");}',
                               expected_failure="BOUNDARY: money")
        self.assertTrue(result["detected"])
        self.assertTrue(result["expected_assertion_seen"])

    def test_custom_assertion_message_detects_only_the_intended_boundary(self):
        result = self.exercise('#[test] fn defect(){assert!(false,"assertion failed: BOUNDARY: fifo");}',
                               expected_failure="BOUNDARY: fifo")
        self.assertTrue(result["detected"])

    def test_wrong_assertion_is_invalid(self):
        result = self.exercise('#[test] fn wrong(){assert_eq!(1,2,"BOUNDARY: unrelated");}',
                               expected_failure="BOUNDARY: money")
        self.assertFalse(result["detected"])
        self.assertFalse(result["expected_assertion_seen"])

    def test_printed_marker_does_not_turn_unrelated_assertion_into_detection(self):
        result = self.exercise('#[test] fn wrong(){println!("BOUNDARY: money");assert_eq!(1,2);}',
                               expected_failure="BOUNDARY: money")
        self.assertFalse(result["detected"])

    def test_required_boundary_counts_must_be_observed(self):
        source = '#[test] fn healthy(){println!("ENGINE_BOUNDARY_HITS {{\\\"money\\\":2}}");assert_eq!(1,1);}'
        result = self.exercise(source, required_hits=("money",))
        self.assertTrue(result["passed"])
        self.assertEqual(result["boundary_hits"], {"money": 2})
        missing = self.exercise("#[test] fn healthy(){assert_eq!(1,1);}", required_hits=("money",))
        self.assertFalse(missing["passed"])
        self.assertEqual(missing["missing_boundary_hits"], ["money"])

    def test_boundary_parser_aggregates_only_positive_integer_counts(self):
        hits, errors = RUNNER.boundary_hits('noise\nENGINE_BOUNDARY_HITS {"money":2}\nENGINE_BOUNDARY_HITS {"money":3,"retry":1}\n')
        self.assertEqual(hits, {"money":5,"retry":1})
        self.assertEqual(errors, [])
        for malformed in ['no JSON', '[]', '{}', '{"x":true}', '{"x":-1}', '{"x":0}', '{"x":1.5}', '{"":1}']:
            with self.subTest(malformed=malformed):
                hits, errors = RUNNER.boundary_hits("ENGINE_BOUNDARY_HITS " + malformed)
                self.assertEqual(hits,{})
                self.assertTrue(errors)

    def test_malformed_boundary_report_invalidates_an_otherwise_healthy_test(self):
        result = self.exercise('#[test] fn healthy(){println!("ENGINE_BOUNDARY_HITS []");assert_eq!(1,1);}')
        self.assertFalse(result["passed"])
        self.assertTrue(result["boundary_hit_errors"])

    def test_compiler_failure_cannot_count_as_detection(self):
        result = self.exercise("#[test] fn broken(){unresolved_function();}")
        self.assertEqual(result["exit_code"], 101)
        self.assertFalse(result["ran_one_test"])
        self.assertFalse(result["detected"])

    def test_zero_tests_cannot_be_a_healthy_baseline(self):
        result = self.exercise("// A misspelled test filter could select no tests.\n")
        self.assertEqual(result["exit_code"], 0)
        self.assertFalse(result["passed"])
        self.assertFalse(result["detected"])

    def test_unrelated_runtime_panic_is_invalid(self):
        result = self.exercise('#[test] fn infrastructure(){panic!("worker disconnected");}')
        self.assertTrue(result["ran_one_test"])
        self.assertFalse(result["detected"])

    def test_hanging_program_is_invalid_and_reaped(self):
        result = self.exercise('#[test] fn hung(){std::fs::write(std::env::var("RUNNER_TEST_PID_PATH").unwrap(), std::process::id().to_string()).unwrap(); std::thread::sleep(std::time::Duration::from_secs(60));}', timeout=5)
        self.assertIsNone(result["exit_code"])
        self.assertFalse(result["detected"])
        self.assertTrue(result["ran_one_test"], "timeout must occur in the test, not the compiler")
        if os.name == "posix":
            self.assertIn("fixture_pid", result)
            self.assertFalse(result["child_still_running"], "Cargo's test child survived the group timeout")

    def test_rendezvous_and_virtual_deadline_failures_are_invalid(self):
        for message in ["assertion failed: never reached publication", "assertion failed: Elapsed(())"]:
            with self.subTest(message=message):
                result = self.exercise(f'#[test] fn infrastructure(){{panic!({json.dumps(message)});}}')
                self.assertFalse(result["detected"])


if __name__ == "__main__":
    unittest.main()
