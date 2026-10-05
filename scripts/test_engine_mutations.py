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
    def exercise(self, source, timeout=30):
        with tempfile.TemporaryDirectory(prefix="mutation-runner-outcome-") as directory:
            root = Path(directory)
            (root / "src").mkdir()
            (root / "Cargo.toml").write_text('[package]\nname="outcome_fixture"\nversion="0.0.0"\nedition="2024"\n[workspace]\n')
            (root / "src/lib.rs").write_text(source)
            env = os.environ.copy()
            env.update(CARGO_TERM_COLOR="never", CARGO_TARGET_DIR=str(root / "build"),
                       RUNNER_TEST_PID_PATH=str(root / "child.pid"))
            result = RUNNER.run(["cargo", "test", "--offline", "--lib", "--", "--nocapture"],
                                root, env, root / "result.log", timeout)
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
