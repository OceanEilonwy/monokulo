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
    expected_failure: str = ""


PORTFOLIO_TEST = "work::tests::properties::combined_portfolio_interactions_have_fixed_positive_controls"
STAGING_TEST = "store::work::tests::every_reorg_staging_cleanup_boundary_survives_reopen"
RECORDED_TEST = "work::tests::properties::every_recorded_ringct_variant_runs_complete_money_histories"
PROOF_SCHEDULE_TEST = "work::tests::properties::concurrency::every_proof_config_shutdown_admission_schedule_recovers"

BLOCK_TEST = "work::blocks::properties::every_late_commit_boundary_is_exercised_with_and_without_staging"
MUTATIONS = (
    Mutation("double-credit-amount", "crates/engine/src/store/mod.rs",
             "sum.saturating_add(v.amount_piconero)",
             "sum.saturating_add(v.amount_piconero).saturating_add(v.amount_piconero)",
             "work::tests::properties::reviewed_mixed_wallet_histories_replay",
             expected_failure="BOUNDARY: independent-amount-ledger"),
    Mutation("accept-stale-block-parent", "crates/engine/src/work/blocks.rs",
             "if s.get_scanned_block_hash(network, block.parent)?",
             "if false && s.get_scanned_block_hash(network, block.parent)?", BLOCK_TEST,
             expected_failure="BOUNDARY: stale-block-publication"),
    Mutation("omit-tenant-order-filter", "crates/engine/src/store/mod.rs",
             "SELECT * FROM orders WHERE id = ?1 AND tenant_id = ?2",
             "SELECT * FROM orders WHERE id = ?1 AND ?2 IS NOT NULL",
             "http::tests::properties::authorization::named_revocation_and_cross_tenant_history_replays",
             expected_failure="BOUNDARY: tenant-authorization"),
    Mutation("lose-payment-recompute-obligation", "crates/engine/migrations/0017_pending_payment_recomputes.sql",
             "SELECT NEW.order_id WHERE NOT EXISTS (",
             "SELECT NEW.order_id WHERE 0 AND NOT EXISTS (", BLOCK_TEST, 2,
             "BOUNDARY: durable-recompute-obligation"),
    Mutation("lose-paid-webhook", "crates/engine/src/scanner.rs",
             "if old_status == new_status {",
             'if old_status == new_status || new_status.as_str() == "paid" {',
             "work::tests::properties::concurrency::every_overlap_admission_order_and_abandoned_caller_recovers",
             expected_failure="BOUNDARY: paid-webhook"),
    Mutation("accept-stale-round-completion", "crates/engine/src/work/scheduler.rs",
             "if self.pending != Some(effect) {",
             "if false && self.pending != Some(effect) {",
             "work::scheduler::properties::late_completions_from_another_round_are_rejected",
             expected_failure="BOUNDARY: stale-round-completion"),
    Mutation("trust-one-spent-vote", "crates/engine/src/daemon_fallback.rs",
             "let status = if votes.iter().all(|vote| *vote == votes[0]) {",
             "let status = if true {", PORTFOLIO_TEST,
             expected_failure="BOUNDARY: independent-void-ledger"),
    Mutation("accept-wrong-scan-window", "crates/engine/src/work/mempool.rs",
             "if lease.generation() != generation {", "if false && lease.generation() != generation {",
             "work::mempool::properties::a_completion_for_another_window_cannot_publish_or_release_its_owner",
             expected_failure="BOUNDARY: wrong-window-owner"),
    Mutation("bypass-proven-settlement", "crates/engine/src/store/mod.rs",
             "!settles_on_proven_blocks()", "false", PORTFOLIO_TEST,
             expected_failure="BOUNDARY: independent-status"),
    Mutation("reverse-conflict-winner", "crates/engine/src/store/conflicts.rs",
             "(height, row.id) < best", "(height, row.id) > best",
             "store::conflicts::tests::the_credit_follows_the_blocks",
             expected_failure="BOUNDARY: canonical-conflict-winner"),
    Mutation("deliver-later-event-before-retry", "crates/engine/src/store/mod.rs",
             "PARTITION BY d.webhook_id, d.order_id ORDER BY d.id",
             "PARTITION BY d.webhook_id, d.order_id ORDER BY d.next_attempt_at_utc, d.id",
             "webhook_delivery::tests::deliveries_are_picked_one_per_order_oldest_first_within_a_stores_share",
             expected_failure="BOUNDARY: retry-fifo"),
    Mutation("retain-reorg-staging-checkpoint", "crates/engine/src/store/work.rs",
             "DELETE FROM partial_block_progress WHERE network = ?1 AND height >= ?2",
             "DELETE FROM partial_block_progress WHERE 0 AND network = ?1 AND height >= ?2", STAGING_TEST,
             expected_failure="BOUNDARY: reorg-staging-cleanup"),
    Mutation("retain-reorg-staging-matches", "crates/engine/src/store/work.rs",
             "DELETE FROM partial_block_matches WHERE network = ?1 AND tenant_id IN",
             "DELETE FROM partial_block_matches WHERE 0 AND network = ?1 AND tenant_id IN", STAGING_TEST,
             expected_failure="BOUNDARY: reorg-staging-matches"),
    Mutation("retain-stale-custody-epoch", "crates/key-custody/src/router.rs",
             "if epoch > previous {", "if false && epoch > previous {",
             "router::properties::backend_epoch_changes_invalidate_only_the_restarted_backend",
             expected_failure="BOUNDARY: stale-custody-epoch"),
)


# Every counter is emitted AFTER the real scenario passes its positive controls.
# These are bounded scenario observations, not instrumented branch coverage.
REQUIRED_HITS = {
    RECORDED_TEST: ("recorded-ringct-history", "recorded-pruned-history", "recorded-whole-history"),
    PORTFOLIO_TEST: (
        "rpc-timeout-cancelled", "custody-error-reached", "sql-denial-reached",
        "all-node-outage-preserves-money-and-cursors", "database-reopened-mid-history",
        "custody-handle-replaced", "unanimous-spent-void-checked", "disputed-spent-retains-funds",
        "void-restored-to-canonical-block", "missing-proof-holds-settlement",
        "mismatching-proof-holds-settlement", "proven-settlement-released",
        "http-503-reached", "http-retry-stable-bytes-and-drained", "database-reopened-final-ledger",
    ),
    STAGING_TEST: ("reorg-staging-reopen-schedules", "reorg-staging-invalidated-at-or-above-fork",
                   "reorg-staging-preserved-below-fork-or-other-network"),
    PROOF_SCHEDULE_TEST: ("worker-proof-config-shutdown-schedules", "worker-custody-replacement-schedules",
                          "worker-missing-anchor-schedules", "worker-mismatching-anchor-schedules"),
}


def boundary_hits(text):
    hits = {}
    errors = []
    for line in text.splitlines():
        if not line.startswith("ENGINE_BOUNDARY_HITS "):
            continue
        try:
            values = json.loads(line.removeprefix("ENGINE_BOUNDARY_HITS "))
            if (not isinstance(values, dict) or not values or
                    any(not isinstance(k, str) or not k or type(v) is not int or v <= 0
                        for k, v in values.items())):
                raise ValueError("expected a nonempty map of boundary names to positive integer counts")
            for key, value in values.items():
                hits[key] = hits.get(key, 0) + value
        except (ValueError, TypeError) as error:
            errors.append(str(error))
    return hits, errors


def test_command(test, feature):
    package = "key-custody" if test.startswith("router::properties::") else "engine"
    command = ["cargo", "test", "-p", package, "--lib", "--locked"]
    if feature == "zmq" and package == "engine":
        command += ["--features", "zmq"]
    return command + [test, "--", "--exact", "--nocapture"]


def positive(value):
    parsed = int(value)
    if parsed < 1:
        raise argparse.ArgumentTypeError("must be positive")
    return parsed


def run(command, cwd, env, log, timeout, expected_failure=None, required_hits=()):
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
    # The marker must be in an assertion panic, not a successful print earlier
    # in the run. Proptest preserves the assertion message in the panic block.
    panic_blocks = re.split(r"\nthread [^\n]+ panicked at [^\n]*:\n", text)[1:]
    intended = expected_failure is None or any(
        expected_failure in block.split("note:", 1)[0].split("test result:", 1)[0]
        and ("assertion" in block or "Test failed:" in block)
        for block in panic_blocks)
    hits, hit_errors = boundary_hits(text)
    missing_hits = [name for name in required_hits if hits.get(name, 0) == 0]
    success = success and not hit_errors and not missing_hits
    detected = (code == 101 and ran and "test result: FAILED. 0 passed; 1 failed" in text
                and assertion and intended and "Elapsed(())" not in text
                and "never reached" not in text)
    return {"command": command, "exit_code": code, "ran_one_test": ran, "passed": success,
            "detected": detected, "expected_failure": expected_failure,
            "expected_assertion_seen": intended, "boundary_hits": hits,
            "boundary_hit_errors": hit_errors, "missing_boundary_hits": missing_hits, "log": str(log)}


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
    report = {"schema_version": 2, "status": "running", "revision": revision, "tracked_patch_sha256": hashlib.sha256(patch).hexdigest(),
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
                for test in dict.fromkeys([m.test for m in MUTATIONS] + list(REQUIRED_HITS)):
                    index = len(baselines)
                    command = test_command(test, feature)
                    result = run(command, tree, env, output / f"baseline-{feature}-{index}.log", args.timeout,
                                 required_hits=REQUIRED_HITS.get(test, ()))
                    baselines[(feature, test)] = result
                    print(f"BASELINE {feature} {test}: {'PASS' if result['passed'] else 'INVALID'}", flush=True)
                    if not result["passed"]:
                        failed = True
            report["baselines"] = [{"features": f, "test": t, **result}
                                   for (f, t), result in baselines.items()]
            for feature in features:
                for mutation in MUTATIONS:
                    entry = {"name": mutation.name, "features": feature, "test": mutation.test,
                             "source": mutation.path, "before": mutation.before, "after": mutation.after,
                             "expected_failure": mutation.expected_failure}
                    path = tree / mutation.path
                    original = path.read_text()
                    try:
                        if not baselines[(feature, mutation.test)]["passed"]:
                            entry["outcome"] = "invalid-baseline"
                        elif original.count(mutation.before) != mutation.occurrences:
                            entry["outcome"] = "invalid-patch"
                        else:
                            path.write_text(original.replace(mutation.before, mutation.after))
                            command = test_command(mutation.test, feature)
                            entry.update(run(command, tree, env,
                                             output / f"{mutation.name}-{feature}.log", args.timeout,
                                             expected_failure=mutation.expected_failure))
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
    report["boundary_observations"] = {
        feature: {test: result["boundary_hits"] for (f, test), result in baselines.items()
                  if f == feature and test in REQUIRED_HITS}
        for feature in features
    }
    report["status"] = "failed" if failed else "passed"
    report["summary"] = {"detected": sum(r["outcome"] == "detected" for r in report["results"]),
                         "expected": len(features) * len(MUTATIONS)}
    persist(output, report)
    print(f"Report: {output / 'report.json'}", flush=True)
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
