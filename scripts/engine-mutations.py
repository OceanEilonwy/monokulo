#!/usr/bin/env python3
"""Prove selected money/scheduling/isolation tests reject named production defects.

Uses a temporary detached worktree, restores each mutant, and never edits the
caller's source. Baselines must pass; compile errors, hangs and zero-test runs
are INVALID, not detections. JSON and full command logs are retained.
"""
import argparse
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import shutil
import subprocess
import tempfile


@dataclass(frozen=True)
class Mutation:
    name: str
    path: str
    before: str
    after: str
    test: str
    occurrences: int = 1


BLOCK_TEST = "work::blocks::properties::every_late_commit_boundary_is_exercised_with_and_without_staging"
MUTATIONS = (
    Mutation("double-credit-amount", "crates/engine/src/store/mod.rs",
             "sum.saturating_add(v.amount_piconero)",
             "sum.saturating_add(v.amount_piconero).saturating_add(v.amount_piconero)",
             "work::tests::properties::reviewed_mixed_wallet_histories_replay"),
    Mutation("accept-stale-block-parent", "crates/engine/src/work/blocks.rs",
             "if s.get_scanned_block_hash(network, block.parent)?",
             "if false && s.get_scanned_block_hash(network, block.parent)?", BLOCK_TEST),
    Mutation("omit-tenant-order-filter", "crates/engine/src/store/mod.rs",
             "SELECT * FROM orders WHERE id = ?1 AND tenant_id = ?2",
             "SELECT * FROM orders WHERE id = ?1 AND ?2 IS NOT NULL",
             "http::tests::properties::authorization::named_revocation_and_cross_tenant_history_replays"),
    Mutation("lose-payment-recompute-obligation", "crates/engine/migrations/0017_pending_payment_recomputes.sql",
             "SELECT NEW.order_id WHERE NOT EXISTS (",
             "SELECT NEW.order_id WHERE 0 AND NOT EXISTS (", BLOCK_TEST, 2),
    Mutation("lose-paid-webhook", "crates/engine/src/scanner.rs",
             "if old_status == new_status {",
             'if old_status == new_status || new_status.as_str() == "paid" {',
             "work::tests::properties::concurrency::every_overlap_admission_order_and_abandoned_caller_recovers"),
    Mutation("accept-stale-round-completion", "crates/engine/src/work/scheduler.rs",
             "if self.pending != Some(effect) {",
             "if false && self.pending != Some(effect) {",
             "work::scheduler::properties::late_completions_from_another_round_are_rejected"),
)


def positive(value):
    parsed = int(value)
    if parsed < 1:
        raise argparse.ArgumentTypeError("must be positive")
    return parsed


def run(command, cwd, env, log, timeout):
    with log.open("w") as output:
        output.write(json.dumps(command) + "\n")
        output.flush()
        try:
            process = subprocess.Popen(command, cwd=cwd, env=env, stdout=output,
                                       stderr=subprocess.STDOUT, start_new_session=os.name == "posix")
            code = process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            # Cargo spawns rustc/test children. Kill this isolated process group
            # too, so an invalid run cannot leave parked workers behind.
            if os.name == "posix":
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            else:
                process.kill()
            process.wait()
            code = None
    text = log.read_text(errors="replace")
    # A compiler/linker failure cannot produce this one-test result. Require the
    # named test's assertion failure, rather than treating any nonzero exit as a kill.
    ran = bool(re.search(r"\brunning 1 test\b", text))
    success = code == 0 and ran and "test result: ok. 1 passed; 0 failed" in text
    assertion = "assertion" in text or "Test failed:" in text
    detected = (code == 101 and ran and "test result: FAILED. 0 passed; 1 failed" in text
                and assertion and "Elapsed(())" not in text
                and "never reached" not in text)
    return {"command": command, "exit_code": code, "ran_one_test": ran, "passed": success,
            "detected": detected, "log": str(log)}


def persist(output, report):
    pending = output / "report.json.tmp"
    pending.write_text(json.dumps(report, indent=2) + "\n")
    pending.replace(output / "report.json")


def seed_value(value):
    parsed = int(value)
    if not 0 <= parsed <= (1 << 64) - 1:
        raise argparse.ArgumentTypeError("must be an unsigned 64-bit value")
    return parsed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--features", choices=("both", "default", "zmq"), default="both")
    parser.add_argument("--cases", type=positive, default=32)
    parser.add_argument("--seed", type=seed_value, default=241)
    parser.add_argument("--timeout", type=positive, default=900,
                        help="wall seconds per compile/test command")
    parser.add_argument("--output", type=Path, default=Path("target/engine-mutations"))
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    output = args.output if args.output.is_absolute() else root / args.output
    output.mkdir(parents=True, exist_ok=True)
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
    patch = subprocess.check_output(["git", "diff", "--binary", "HEAD"], cwd=root)
    # Snapshot tracked changes too, allowing this runner to validate code before
    # its commit. New engine modules are the only untracked build inputs copied.
    untracked = subprocess.check_output(["git", "ls-files", "--others", "--exclude-standard", "-z",
                                         "crates/engine"], cwd=root).decode().split("\0")
    report = {"schema_version": 1, "status": "running", "revision": revision, "tracked_patch_sha256": hashlib.sha256(patch).hexdigest(),
              "cases": args.cases, "seed": args.seed, "results": []}
    persist(output, report)
    env = os.environ.copy()
    env.update(PROPTEST_CASES=str(args.cases), PROPTEST_RNG_SEED=str(args.seed),
               CARGO_TERM_COLOR="never", CARGO_TARGET_DIR=str(output / "build"))
    # Prevent caller crash-test settings from parking a baseline indefinitely.
    for key in tuple(env):
        if key.startswith("MONOKULO_PROPERTY_CRASH_") or key.startswith("MONOKULO_SCANNER_CRASH_"):
            del env[key]
    features = ("default", "zmq") if args.features == "both" else (args.features,)
    failed = False
    with tempfile.TemporaryDirectory(prefix="engine-mutations-") as temporary:
        tree = Path(temporary) / "source"
        subprocess.run(["git", "worktree", "add", "--detach", str(tree), revision],
                       cwd=root, check=True, stdout=subprocess.DEVNULL)
        try:
            if patch:
                subprocess.run(["git", "apply", "--binary", "-"], cwd=tree,
                               input=patch, check=True)
            for relative in filter(None, untracked):
                source = root / relative
                if source.is_file():
                    destination = tree / relative
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(source, destination)
            # Check ALL healthy baselines first. No mutant is allowed to inherit
            # failure from an earlier mutant or masquerade as a broken baseline.
            baselines = {}
            for feature in features:
                for test in dict.fromkeys(m.test for m in MUTATIONS):
                    index = len(baselines)
                    command = ["cargo", "test", "-p", "engine", "--lib", "--locked"]
                    if feature == "zmq":
                        command += ["--features", "zmq"]
                    command += [test, "--", "--exact", "--nocapture"]
                    result = run(command, tree, env, output / f"baseline-{feature}-{index}.log", args.timeout)
                    baselines[(feature, test)] = result
                    print(f"BASELINE {feature} {test}: {'PASS' if result['passed'] else 'INVALID'}", flush=True)
                    if not result["passed"]:
                        failed = True
            report["baselines"] = [{"features": f, "test": t, **result}
                                   for (f, t), result in baselines.items()]
            for feature in features:
                for mutation in MUTATIONS:
                    entry = {"name": mutation.name, "features": feature, "test": mutation.test,
                             "source": mutation.path, "before": mutation.before, "after": mutation.after}
                    path = tree / mutation.path
                    original = path.read_text()
                    try:
                        if not baselines[(feature, mutation.test)]["passed"]:
                            entry["outcome"] = "invalid-baseline"
                        elif original.count(mutation.before) != mutation.occurrences:
                            entry["outcome"] = "invalid-patch"
                        else:
                            path.write_text(original.replace(mutation.before, mutation.after))
                            command = ["cargo", "test", "-p", "engine", "--lib", "--locked"]
                            if feature == "zmq":
                                command += ["--features", "zmq"]
                            command += [mutation.test, "--", "--exact", "--nocapture"]
                            entry.update(run(command, tree, env,
                                             output / f"{mutation.name}-{feature}.log", args.timeout))
                            entry["outcome"] = ("detected" if entry["detected"] else
                                                "survived" if entry["passed"] else "invalid-run")
                        failed |= entry["outcome"] != "detected"
                        report["results"].append(entry)
                        persist(output, report)
                        print(f"{entry['outcome'].upper()} {feature} {mutation.name}", flush=True)
                    finally:
                        path.write_text(original)
        finally:
            subprocess.run(["git", "worktree", "remove", "--force", str(tree)], cwd=root, check=True)
    report["status"] = "failed" if failed else "passed"
    report["summary"] = {"detected": sum(r["outcome"] == "detected" for r in report["results"]),
                         "expected": len(features) * len(MUTATIONS)}
    persist(output, report)
    print(f"Report: {output / 'report.json'}", flush=True)
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
