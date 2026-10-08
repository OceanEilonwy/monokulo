//! `cargo xtask engine mutations`: proves selected money, scheduling and isolation
//! tests reject named production defects.
//!
//! It uses a temporary detached worktree, restores each mutant, and never
//! edits the caller's source. Baselines must pass; compile errors, hangs and
//! zero-test runs are INVALID, not detections. The JSON report and every
//! command's log are kept under the output folder.

use crate::exploration::Build;
use crate::support::{at, wait_with_deadline, write_json, Exit, OnTimeout};
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    env, fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::LazyLock,
    time::Duration,
};

/// One named defect: `before` replaced by `after` in `path` must make `test`
/// fail with `expected_failure` in its assertion.
struct Mutation {
    name: &'static str,
    path: &'static str,
    before: &'static str,
    after: &'static str,
    test: &'static str,
    occurrences: usize,
    expected_failure: &'static str,
}

const fn m(
    name: &'static str,
    path: &'static str,
    before: &'static str,
    after: &'static str,
    test: &'static str,
    expected_failure: &'static str,
) -> Mutation {
    Mutation {
        name,
        path,
        before,
        after,
        test,
        occurrences: 1,
        expected_failure,
    }
}

const PORTFOLIO_TEST: &str =
    "work::tests::properties::combined_portfolio_interactions_have_fixed_positive_controls";
const STAGING_TEST: &str =
    "store::work::tests::every_reorg_staging_cleanup_boundary_survives_reopen";
const RECORDED_TEST: &str =
    "work::tests::properties::every_recorded_ringct_variant_runs_complete_money_histories";
const PROOF_SCHEDULE_TEST: &str =
    "work::tests::properties::concurrency::every_proof_config_shutdown_admission_schedule_recovers";
const BLOCK_TEST: &str =
    "work::blocks::properties::every_late_commit_boundary_is_exercised_with_and_without_staging";
const STATUS_GRID: &str = "status::properties::status_decision_boundaries_match_independent_grid";
const RETRY_EXPIRY: &str =
    "work::scheduler::properties::retry_expiry_boundaries_use_production_upkeep";
const RETRY_GRID: &str =
    "work::scheduler::properties::retry_policy_decision_grid_checks_free_attempts_and_deadlines";
const RESERVATION_GRID: &str =
    "work::mempool::properties::reservation_decision_grid_checks_completed_busy_and_changed_windows";

const MUTATIONS: &[Mutation] = &[
    m("never-forget-retries", "crates/engine/src/work/retry.rs",
      "now.saturating_sub(self.last_failure) >= Self::FORGET_AFTER",
      "false && now.saturating_sub(self.last_failure) >= Self::FORGET_AFTER", RETRY_EXPIRY, "BOUNDARY: retry-expiry"),
    // Systematic comparison/guard perturbations at critical decision boundaries.
    m("funding-boundary-strict", "crates/engine/src/status.rs",
      "if total >= inputs.xmr_amount_piconero {", "if total > inputs.xmr_amount_piconero {", STATUS_GRID, "BOUNDARY: status-decision-grid"),
    m("confirmation-boundary-strict", "crates/engine/src/status.rs",
      "min_confirmations >= inputs.confirmations_required", "min_confirmations > inputs.confirmations_required", STATUS_GRID,
      "BOUNDARY: status-decision-grid"),
    m("expiry-boundary-inclusive", "crates/engine/src/status.rs",
      "inputs.now > inputs.expires_at", "inputs.now >= inputs.expires_at", STATUS_GRID, "BOUNDARY: status-decision-grid"),
    m("overpayment-boundary-inclusive", "crates/engine/src/status.rs",
      "if total > inputs.xmr_amount_piconero {", "if total >= inputs.xmr_amount_piconero {", STATUS_GRID, "BOUNDARY: status-decision-grid"),
    m("retry-expiry-late", "crates/engine/src/work/retry.rs",
      "now.saturating_sub(self.last_failure) >= Self::FORGET_AFTER", "now.saturating_sub(self.last_failure) > Self::FORGET_AFTER",
      RETRY_EXPIRY, "BOUNDARY: retry-expiry"),
    m("retry-expiry-early", "crates/engine/src/work/retry.rs",
      "now.saturating_sub(self.last_failure) >= Self::FORGET_AFTER",
      "now.saturating_sub(self.last_failure) >= Self::FORGET_AFTER.saturating_sub(Duration::from_secs(1))",
      RETRY_EXPIRY, "BOUNDARY: retry-expiry"),
    m("retry-deadline-inclusive", "crates/engine/src/work/retry.rs",
      "now < self.retry_at", "now <= self.retry_at", RETRY_GRID, "BOUNDARY: retry-policy-grid"),
    m("retry-second-attempt-delayed", "crates/engine/src/work/retry.rs",
      "if failures <= 2 {", "if failures < 2 {", RETRY_GRID, "BOUNDARY: retry-policy-grid"),
    m("rescan-completed-window", "crates/engine/src/work/reservations.rs",
      "if self.completed(txid, tenant) == Some(generation) {", "if false && self.completed(txid, tenant) == Some(generation) {",
      RESERVATION_GRID, "BOUNDARY: reservation-decision-grid"),
    m("admit-concurrent-window-owner", "crates/engine/src/work/reservations.rs",
      "if !self.in_flight.insert(key) {", "if false && !self.in_flight.insert(key) {", RESERVATION_GRID,
      "BOUNDARY: reservation-decision-grid"),
    m("double-credit-amount", "crates/engine/src/store/mod.rs",
      "sum.saturating_add(v.amount_piconero)", "sum.saturating_add(v.amount_piconero).saturating_add(v.amount_piconero)",
      "work::tests::properties::reviewed_mixed_wallet_histories_replay", "BOUNDARY: independent-amount-ledger"),
    m("accept-stale-block-parent", "crates/engine/src/work/blocks.rs",
      "if s.get_scanned_block_hash(network, block.parent)?", "if false && s.get_scanned_block_hash(network, block.parent)?",
      BLOCK_TEST, "BOUNDARY: stale-block-publication"),
    m("omit-tenant-order-filter", "crates/engine/src/store/mod.rs",
      "SELECT * FROM orders WHERE id = ?1 AND tenant_id = ?2", "SELECT * FROM orders WHERE id = ?1 AND ?2 IS NOT NULL",
      "http::tests::properties::authorization::named_revocation_and_cross_tenant_history_replays", "BOUNDARY: tenant-authorization"),
    Mutation {
        name: "lose-payment-recompute-obligation",
        path: "crates/engine/migrations/0017_pending_payment_recomputes.sql",
        before: "SELECT NEW.order_id WHERE NOT EXISTS (",
        after: "SELECT NEW.order_id WHERE 0 AND NOT EXISTS (",
        test: BLOCK_TEST,
        occurrences: 2,
        expected_failure: "BOUNDARY: durable-recompute-obligation",
    },
    m("lose-paid-webhook", "crates/engine/src/scanner.rs",
      "if old_status == new_status {", "if old_status == new_status || new_status.as_str() == \"paid\" {",
      "work::tests::properties::concurrency::every_overlap_admission_order_and_abandoned_caller_recovers", "BOUNDARY: paid-webhook"),
    m("accept-stale-round-completion", "crates/engine/src/work/scheduler.rs",
      "if self.pending != Some(effect) {", "if false && self.pending != Some(effect) {",
      "work::scheduler::properties::late_completions_from_another_round_are_rejected", "BOUNDARY: stale-round-completion"),
    m("trust-one-spent-vote", "crates/engine/src/daemon_fallback.rs",
      "let status = if votes.iter().all(|vote| *vote == votes[0]) {", "let status = if true {", PORTFOLIO_TEST,
      "BOUNDARY: independent-void-ledger"),
    m("accept-wrong-scan-window", "crates/engine/src/work/mempool.rs",
      "if lease.generation() != generation {", "if false && lease.generation() != generation {",
      "work::mempool::properties::a_completion_for_another_window_cannot_publish_or_release_its_owner", "BOUNDARY: wrong-window-owner"),
    m("bypass-proven-settlement", "crates/engine/src/store/mod.rs",
      "!settles_on_proven_blocks()", "false", PORTFOLIO_TEST, "BOUNDARY: independent-status"),
    m("reverse-conflict-winner", "crates/engine/src/store/conflicts.rs",
      "(height, row.id) < best", "(height, row.id) > best", "store::conflicts::tests::the_credit_follows_the_blocks",
      "BOUNDARY: canonical-conflict-winner"),
    m("deliver-later-event-before-retry", "crates/engine/src/store/mod.rs",
      "PARTITION BY d.webhook_id, d.order_id ORDER BY d.id",
      "PARTITION BY d.webhook_id, d.order_id ORDER BY d.next_attempt_at_utc, d.id",
      "webhook_delivery::tests::deliveries_are_picked_one_per_order_oldest_first_within_a_stores_share", "BOUNDARY: retry-fifo"),
    m("retain-reorg-staging-checkpoint", "crates/engine/src/store/work.rs",
      "DELETE FROM partial_block_progress WHERE network = ?1 AND height >= ?2",
      "DELETE FROM partial_block_progress WHERE 0 AND network = ?1 AND height >= ?2", STAGING_TEST, "BOUNDARY: reorg-staging-cleanup"),
    m("retain-reorg-staging-matches", "crates/engine/src/store/work.rs",
      "DELETE FROM partial_block_matches WHERE network = ?1 AND tenant_id IN",
      "DELETE FROM partial_block_matches WHERE 0 AND network = ?1 AND tenant_id IN", STAGING_TEST, "BOUNDARY: reorg-staging-matches"),
    m("retain-stale-custody-epoch", "crates/key-custody/src/router.rs",
      "if epoch > previous {", "if false && epoch > previous {",
      "router::properties::backend_epoch_changes_invalidate_only_the_restarted_backend", "BOUNDARY: stale-custody-epoch"),
    m("new-arrivals-starve-old-windows", "crates/engine/src/work/mempool.rs",
      "rotation.update(pool_txids);",
      "rotation.update(pool_txids); rotation.queue.make_contiguous().sort_by_key(|id| (!state.is_new(id), id.clone()));",
      "work::mempool::properties::scale::expanded_windows_are_served_during_continuous_new_transaction_floods",
      "BOUNDARY: sustained-arrival-starvation"),
    m("share-transaction-tenant-cursor", "crates/engine/src/work/mempool.rs",
      "async fn tenant_page(round: &Round<'_>, txid: &str) -> Result<Vec<TenantWindow>, ScannerError> {",
      "async fn tenant_page(round: &Round<'_>, txid: &str) -> Result<Vec<TenantWindow>, ScannerError> { let shared = format!(\"global-{}\", txid.len()); let txid = shared.as_str();",
      "work::mempool::properties::scale::transaction_cursors_cannot_be_shared_between_three_transactions",
      "BOUNDARY: transaction-tenant-fairness"),
];

/// Every counter is emitted AFTER the real scenario passes its positive
/// controls. These are bounded scenario observations, not instrumented
/// branch coverage.
const REQUIRED_HITS: &[(&str, &[&str])] = &[
    (
        RECORDED_TEST,
        &[
            "recorded-ringct-history",
            "recorded-pruned-history",
            "recorded-whole-history",
        ],
    ),
    (
        PORTFOLIO_TEST,
        &[
            "rpc-timeout-cancelled",
            "custody-error-reached",
            "sql-denial-reached",
            "engine-rpc-timeout-cancelled",
            "engine-custody-error-reached",
            "engine-fault-payment-recovered",
            "all-node-outage-preserves-money-and-cursors",
            "connection-reopened-mid-history",
            "custody-handle-replaced",
            "unanimous-spent-void-checked",
            "disputed-spent-retains-funds",
            "void-restored-to-canonical-block",
            "missing-proof-holds-settlement",
            "mismatching-proof-holds-settlement",
            "proven-settlement-released",
            "http-503-reached",
            "http-retry-stable-bytes-and-drained",
            "connection-reopened-final-ledger",
        ],
    ),
    (
        STAGING_TEST,
        &[
            "reorg-staging-reopen-schedules",
            "reorg-staging-invalidated-at-or-above-fork",
            "reorg-staging-preserved-below-fork-or-other-network",
        ],
    ),
    (
        PROOF_SCHEDULE_TEST,
        &[
            "worker-proof-config-shutdown-schedules",
            "worker-custody-replacement-schedules",
            "worker-missing-anchor-schedules",
            "worker-mismatching-anchor-schedules",
        ],
    ),
];

fn required_hits(test: &str) -> &'static [&'static str] {
    REQUIRED_HITS
        .iter()
        .find(|(t, _)| *t == test)
        .map_or(&[], |(_, hits)| hits)
}

pub(crate) const HELP: &str = "\
        engine mutations [--features both|default|zmq] [--cases N] [--seed N] [--timeout SECONDS]\n\
                         [--output DIR] [--only NAME]...\n\
                      Prove the engine's named-defect tests catch each defect: every mutant in a detached\n\
                      worktree, against passing baselines (target/engine-mutations/report.json)";

/// The `ENGINE_BOUNDARY_HITS {json}` lines a test printed, added up, and what
/// was wrong with any malformed ones.
fn boundary_hits(text: &str) -> (BTreeMap<String, u64>, Vec<String>) {
    let (mut hits, mut errors) = (BTreeMap::new(), Vec::new());
    for line in text.lines() {
        let Some(json) = line.strip_prefix("ENGINE_BOUNDARY_HITS ") else {
            continue;
        };
        let counts: Option<BTreeMap<String, u64>> = match serde_json::from_str::<Value>(json) {
            Ok(Value::Object(map)) if !map.is_empty() => map
                .into_iter()
                .map(|(k, v)| match v.as_u64() {
                    Some(n) if !k.is_empty() && n > 0 => Some((k, n)),
                    _ => None,
                })
                .collect(),
            Ok(_) => None,
            Err(e) => {
                errors.push(e.to_string());
                continue;
            }
        };
        match counts {
            Some(map) => {
                for (k, v) in map {
                    *hits.entry(k).or_insert(0) += v;
                }
            }
            None => errors.push(
                "expected a nonempty map of boundary names to positive integer counts".into(),
            ),
        }
    }
    (hits, errors)
}

/// The command that runs one named test of the package it lives in.
fn test_command(test: &str, build: Build) -> Vec<String> {
    let package = if test.starts_with("router::properties::") {
        "key-custody"
    } else {
        "engine"
    };
    let mut command: Vec<String> = ["cargo", "test", "-p", package, "--lib", "--locked"]
        .map(String::from)
        .to_vec();
    if build == Build::Zmq && package == "engine" {
        command.extend(["--features".into(), build.features().into()]);
    }
    command.extend([
        test.into(),
        "--".into(),
        "--exact".into(),
        "--nocapture".into(),
    ]);
    command
}

static RAN_ONE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\brunning 1 test\b").unwrap());
static PANIC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\nthread [^\n]+ panicked at [^\n]*:\n").unwrap());

/// Where and how every test command of a run goes: its checkout, the
/// variables it adds and removes, and how long one command may take.
struct Context<'a> {
    cwd: &'a Path,
    env: &'a BTreeMap<String, String>,
    removed: &'a [String],
    timeout: u64,
}

/// What one test run showed: a healthy pass, the intended assertion
/// failing, or neither.
#[derive(Serialize, Clone)]
struct Verdict {
    command: Vec<String>,
    /// `None` when the watchdog stopped it.
    exit_code: Option<Exit>,
    ran_one_test: bool,
    passed: bool,
    detected: bool,
    expected_failure: Option<String>,
    expected_assertion_seen: bool,
    boundary_hits: BTreeMap<String, u64>,
    boundary_hit_errors: Vec<String>,
    missing_boundary_hits: Vec<String>,
    log: String,
}

/// Runs `command`, logged to `log`, and judges what it showed.
fn run(
    command: &[String],
    context: &Context,
    log: &Path,
    expected_failure: Option<&str>,
    required: &[&str],
) -> io::Result<Verdict> {
    let Context {
        cwd,
        env,
        removed,
        timeout,
    } = *context;
    let mut file = fs::File::create(log).map_err(|e| at(log, e))?;
    io::Write::write_all(
        &mut file,
        format!("{}\n", serde_json::to_string(command)?).as_bytes(),
    )?;
    let mut process = Command::new(&command[0]);
    process
        .args(&command[1..])
        .current_dir(cwd)
        .envs(env)
        .stdout(file.try_clone()?)
        .stderr(file)
        .stdin(Stdio::null());
    for key in removed {
        process.env_remove(key);
    }
    #[cfg(unix)]
    {
        // Cargo spawns rustc and test children: a group of their own lets a
        // timeout stop them all, so an invalid run leaves no parked workers.
        use std::os::unix::process::CommandExt;
        process.process_group(0);
    }
    let mut child = process.spawn()?;
    let exit_code = wait_with_deadline(
        &mut child,
        Duration::from_secs(timeout),
        OnTimeout::KillGroup,
    )?
    .map(Exit::of);
    let text = String::from_utf8_lossy(&fs::read(log)?).into_owned();
    // A compiler or linker failure cannot produce this one-test result.
    // Require the named test's assertion failure rather than treating any
    // nonzero exit as a kill.
    let ran = RAN_ONE.is_match(&text);
    let assertion = text.contains("assertion") || text.contains("Test failed:");
    // The marker must be in an assertion panic, not a successful print
    // earlier in the run. Proptest keeps the assertion message in the panic.
    let intended = expected_failure.is_none_or(|expected| {
        PANIC.split(&text).skip(1).any(|block| {
            let head = block
                .split("note:")
                .next()
                .unwrap_or("")
                .split("test result:")
                .next()
                .unwrap_or("");
            head.contains(expected)
                && (block.contains("assertion") || block.contains("Test failed:"))
        })
    });
    let (hits, hit_errors) = boundary_hits(&text);
    let missing: Vec<String> = required
        .iter()
        .filter(|n| hits.get(**n).copied().unwrap_or(0) == 0)
        .map(|n| (*n).to_string())
        .collect();
    let passed = exit_code == Some(Exit::SUCCESS)
        && ran
        && text.contains("test result: ok. 1 passed; 0 failed")
        && hit_errors.is_empty()
        && missing.is_empty();
    let detected = exit_code == Some(TEST_FAILED)
        && ran
        && text.contains("test result: FAILED. 0 passed; 1 failed")
        && assertion
        && intended
        && !text.contains("Elapsed(())")
        && !text.contains("never reached");
    Ok(Verdict {
        command: command.to_vec(),
        exit_code,
        ran_one_test: ran,
        passed,
        detected,
        expected_failure: expected_failure.map(str::to_string),
        expected_assertion_seen: intended,
        boundary_hits: hits,
        boundary_hit_errors: hit_errors,
        missing_boundary_hits: missing,
        log: log.display().to_string(),
    })
}

/// The test harness's code for a test that failed.
const TEST_FAILED: Exit = Exit::new(101);

fn git(root: &Path, args: &[&str]) -> io::Result<Vec<u8>> {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(output.stdout)
}

struct Options {
    builds: Vec<Build>,
    cases: u64,
    seed: u64,
    timeout: u64,
    output: PathBuf,
    only: Vec<String>,
}

fn parse(args: &[&str]) -> io::Result<Options> {
    let mut o = Options {
        builds: vec![Build::Default, Build::Zmq],
        cases: 32,
        seed: 241,
        timeout: 900,
        output: PathBuf::from("target/engine-mutations"),
        only: Vec::new(),
    };
    let bad =
        |what: &str| io::Error::new(io::ErrorKind::InvalidInput, format!("mutations: {what}"));
    let positive = |v: &str| {
        v.parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| bad(&format!("{v} must be positive")))
    };
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let value = *rest
            .next()
            .ok_or_else(|| bad(&format!("{flag} needs a value")))?;
        match *flag {
            "--features" => {
                o.builds = match value {
                    "both" => vec![Build::Default, Build::Zmq],
                    "default" => vec![Build::Default],
                    "zmq" => vec![Build::Zmq],
                    _ => return Err(bad("--features is both, default or zmq")),
                }
            }
            "--cases" => o.cases = positive(value)?,
            "--seed" => {
                o.seed = value
                    .parse()
                    .map_err(|_| bad("--seed must be an unsigned 64-bit value"))?;
            }
            "--timeout" => o.timeout = positive(value)?,
            "--output" => o.output = PathBuf::from(value),
            "--only" => {
                if !MUTATIONS.iter().any(|m| m.name == value) {
                    return Err(bad(&format!("no defect named {value}")));
                }
                o.only.push(value.to_string());
            }
            _ => return Err(bad(&format!("unknown option {flag}"))),
        }
    }
    Ok(o)
}

/// The caller's source as it is now: the commit, the tracked changes on top
/// of it, and the new engine modules (the only untracked build inputs copied).
struct Snapshot {
    revision: String,
    patch: Vec<u8>,
    untracked: Vec<String>,
}

fn snapshot(root: &Path) -> io::Result<Snapshot> {
    let revision = String::from_utf8_lossy(&git(root, &["rev-parse", "HEAD"])?)
        .trim()
        .to_string();
    let patch = git(root, &["diff", "--binary", "HEAD"])?;
    let untracked = git(
        root,
        &[
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
            "crates/engine",
        ],
    )?;
    let untracked = untracked
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect();
    Ok(Snapshot {
        revision,
        patch,
        untracked,
    })
}

/// A detached worktree of the snapshot in a scratch folder of its own, both
/// removed when dropped, so neither outlives a run that failed.
struct Worktree {
    root: PathBuf,
    scratch: PathBuf,
    tree: PathBuf,
    removed: bool,
}

impl Worktree {
    fn new(root: &Path, source: &Snapshot) -> io::Result<Self> {
        let scratch = env::temp_dir().join(format!("engine-mutations-{}", std::process::id()));
        let _ = fs::remove_dir_all(&scratch);
        fs::create_dir_all(&scratch).map_err(|e| at(&scratch, e))?;
        let worktree = Worktree {
            root: root.to_path_buf(),
            tree: scratch.join("source"),
            scratch,
            removed: false,
        };
        git(
            root,
            &[
                "worktree",
                "add",
                "--detach",
                &worktree.tree.display().to_string(),
                &source.revision,
            ],
        )?;
        if !source.patch.is_empty() {
            let mut apply = Command::new("git")
                .args(["apply", "--binary", "-"])
                .current_dir(&worktree.tree)
                .stdin(Stdio::piped())
                .spawn()?;
            if let Some(stdin) = apply.stdin.as_mut() {
                io::Write::write_all(stdin, &source.patch)?;
            }
            drop(apply.stdin.take());
            if !apply.wait()?.success() {
                return Err(io::Error::other("git apply failed"));
            }
        }
        for relative in &source.untracked {
            let from = root.join(relative);
            if from.is_file() {
                let to = worktree.tree.join(relative);
                if let Some(parent) = to.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(&from, &to).map_err(|e| at(&from, e))?;
            }
        }
        Ok(worktree)
    }

    fn remove_tree(&self) -> io::Result<()> {
        git(
            &self.root,
            &[
                "worktree",
                "remove",
                "--force",
                &self.tree.display().to_string(),
            ],
        )
        .map(|_| ())
    }

    /// Removes the worktree now, so a failure to do so is reported.
    fn remove(mut self) -> io::Result<()> {
        self.remove_tree()?;
        self.removed = true;
        Ok(())
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        if !self.removed {
            let _ = self.remove_tree();
        }
        let _ = fs::remove_dir_all(&self.scratch);
    }
}

/// One test's healthy run under one build.
struct Baseline {
    build: Build,
    test: &'static str,
    verdict: Verdict,
}

impl Baseline {
    fn to_json(&self) -> Value {
        let mut entry = json!({"features": self.build, "test": self.test});
        extend(&mut entry, &self.verdict);
        entry
    }
}

fn extend(entry: &mut Value, verdict: &Verdict) {
    if let (Some(fields), Ok(Value::Object(from))) =
        (entry.as_object_mut(), serde_json::to_value(verdict))
    {
        fields.extend(from);
    }
}

/// Every healthy baseline, checked first: no mutant may inherit failure
/// from an earlier one or pass for a broken baseline.
fn baselines(
    o: &Options,
    selected: &[&Mutation],
    context: &Context,
    output: &Path,
) -> io::Result<Vec<Baseline>> {
    let mut tests: Vec<&'static str> = Vec::new();
    let required: Vec<&'static str> = if o.only.is_empty() {
        REQUIRED_HITS.iter().map(|(t, _)| *t).collect()
    } else {
        Vec::new()
    };
    for test in selected.iter().map(|m| m.test).chain(required) {
        if !tests.contains(&test) {
            tests.push(test);
        }
    }
    let mut found = Vec::new();
    for &build in &o.builds {
        for &test in &tests {
            let log = output.join(format!("baseline-{build}-{}.log", found.len()));
            let verdict = run(
                &test_command(test, build),
                context,
                &log,
                None,
                required_hits(test),
            )?;
            println!(
                "BASELINE {build} {test}: {}",
                if verdict.passed { "PASS" } else { "INVALID" }
            );
            found.push(Baseline {
                build,
                test,
                verdict,
            });
        }
    }
    Ok(found)
}

/// What a mutant's run proved.
#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Outcome {
    /// The named test failed with the intended assertion.
    Detected,
    /// The named test still passed.
    Survived,
    /// The test's baseline did not pass, so the mutant proves nothing.
    InvalidBaseline,
    /// The source no longer has the text the mutation replaces.
    InvalidPatch,
    /// Neither a pass nor the intended failure: a compile error, hang or panic.
    InvalidRun,
}

impl Outcome {
    fn name(self) -> &'static str {
        match self {
            Outcome::Detected => "detected",
            Outcome::Survived => "survived",
            Outcome::InvalidBaseline => "invalid-baseline",
            Outcome::InvalidPatch => "invalid-patch",
            Outcome::InvalidRun => "invalid-run",
        }
    }
}

/// Runs one mutant: the source changed, the named test run, the source
/// restored whatever happened.
fn mutant(
    mutation: &Mutation,
    build: Build,
    context: &Context,
    output: &Path,
    baseline_ok: bool,
) -> io::Result<(Outcome, Option<Verdict>)> {
    if !baseline_ok {
        return Ok((Outcome::InvalidBaseline, None));
    }
    let path = context.cwd.join(mutation.path);
    let original = fs::read_to_string(&path).map_err(|e| at(&path, e))?;
    if original.matches(mutation.before).count() != mutation.occurrences {
        return Ok((Outcome::InvalidPatch, None));
    }
    fs::write(&path, original.replace(mutation.before, mutation.after))?;
    let log = output.join(format!("{}-{build}.log", mutation.name));
    let verdict = run(
        &test_command(mutation.test, build),
        context,
        &log,
        Some(mutation.expected_failure),
        &[],
    );
    fs::write(&path, &original)?;
    let verdict = verdict?;
    let outcome = if verdict.detected {
        Outcome::Detected
    } else if verdict.passed {
        Outcome::Survived
    } else {
        Outcome::InvalidRun
    };
    Ok((outcome, Some(verdict)))
}

/// Every selected mutant under every build, each added to the report as it
/// finishes; whether all were detected.
fn mutants(
    o: &Options,
    selected: &[&Mutation],
    context: &Context,
    output: &Path,
    baselines: &[Baseline],
    report: &mut Value,
) -> io::Result<bool> {
    let mut all_detected = true;
    for &build in &o.builds {
        for mutation in selected {
            let baseline_ok = baselines
                .iter()
                .any(|b| b.build == build && b.test == mutation.test && b.verdict.passed);
            let (outcome, verdict) = mutant(mutation, build, context, output, baseline_ok)?;
            let mut entry = json!({"name": mutation.name, "features": build, "test": mutation.test, "source": mutation.path,
                "before": mutation.before, "after": mutation.after, "expected_failure": mutation.expected_failure});
            if let Some(verdict) = &verdict {
                extend(&mut entry, verdict);
            }
            entry["outcome"] = json!(outcome);
            all_detected &= outcome == Outcome::Detected;
            if let Some(results) = report["results"].as_array_mut() {
                results.push(entry);
            }
            write_json(&output.join("report.json"), report)?;
            println!(
                "{} {build} {}",
                outcome.name().to_uppercase(),
                mutation.name
            );
        }
    }
    Ok(all_detected)
}

/// Runs the catalogue (or `--only` some of it) against the checkout at `root`.
pub(crate) fn mutations(root: &Path, args: &[&str]) -> io::Result<Exit> {
    let o = parse(args)?;
    let selected: Vec<&Mutation> = MUTATIONS
        .iter()
        .filter(|m| o.only.is_empty() || o.only.iter().any(|n| n == m.name))
        .collect();
    let output = if o.output.is_absolute() {
        o.output.clone()
    } else {
        root.join(&o.output)
    };
    fs::create_dir_all(&output)?;
    let source = snapshot(root)?;
    let mut report = json!({
        "schema_version": 2, "status": "running", "revision": source.revision, "tracked_patch_sha256": hex::encode(Sha256::digest(&source.patch)),
        "cases": o.cases, "seed": o.seed, "selected_mutations": selected.iter().map(|m| m.name).collect::<Vec<_>>(), "results": [],
    });
    let report_path = output.join("report.json");
    write_json(&report_path, &report)?;
    let env: BTreeMap<String, String> = [
        ("PROPTEST_CASES", o.cases.to_string()),
        ("PROPTEST_RNG_SEED", o.seed.to_string()),
        ("CARGO_TERM_COLOR", "never".into()),
        (
            "CARGO_TARGET_DIR",
            output.join("build").display().to_string(),
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    // A caller's crash-test settings would park a baseline indefinitely.
    let removed: Vec<String> = env::vars()
        .map(|(k, _)| k)
        .filter(|k| {
            k.starts_with("MONOKULO_PROPERTY_CRASH_") || k.starts_with("MONOKULO_SCANNER_CRASH_")
        })
        .collect();
    let worktree = Worktree::new(root, &source)?;
    let context = Context {
        cwd: &worktree.tree,
        env: &env,
        removed: &removed,
        timeout: o.timeout,
    };
    let baselines = baselines(&o, &selected, &context, &output)?;
    let baselines_ok = baselines.iter().all(|b| b.verdict.passed);
    report["baselines"] = baselines.iter().map(Baseline::to_json).collect();
    let detected_all = mutants(&o, &selected, &context, &output, &baselines, &mut report)?;
    worktree.remove()?;
    report["boundary_observations"] = o
        .builds
        .iter()
        .map(|build| {
            let observed: Map<String, Value> = baselines
                .iter()
                .filter(|b| b.build == *build && REQUIRED_HITS.iter().any(|(r, _)| *r == b.test))
                .map(|b| (b.test.to_string(), json!(b.verdict.boundary_hits)))
                .collect();
            (build.name().to_string(), Value::Object(observed))
        })
        .collect::<Map<_, _>>()
        .into();
    let passed = baselines_ok && detected_all;
    report["status"] = if passed { "passed" } else { "failed" }.into();
    let detected = report["results"].as_array().map_or(0, |results| {
        results
            .iter()
            .filter(|r| r["outcome"] == Outcome::Detected.name())
            .count()
    });
    report["summary"] = json!({"detected": detected, "expected": o.builds.len() * selected.len()});
    write_json(&report_path, &report)?;
    println!("Report: {}", report_path.display());
    Ok(Exit::passed(passed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::Scratch;

    /// Runs a one-file Cargo crate's tests through `run`, as a mutant's would be.
    fn exercise(source: &str, timeout: u64, expected: Option<&str>, required: &[&str]) -> Value {
        let scratch = Scratch::new("mutation-runner-outcome");
        let root = scratch.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\nname=\"outcome_fixture\"\nversion=\"0.0.0\"\nedition=\"2024\"\n[workspace]\n").unwrap();
        fs::write(root.join("src/lib.rs"), source).unwrap();
        let env: BTreeMap<String, String> = [
            ("CARGO_TERM_COLOR", "never".to_string()),
            ("CARGO_TARGET_DIR", root.join("build").display().to_string()),
            (
                "RUNNER_TEST_PID_PATH",
                root.join("child.pid").display().to_string(),
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let command: Vec<String> = ["cargo", "test", "--offline", "--lib", "--", "--nocapture"]
            .map(String::from)
            .to_vec();
        let context = Context {
            cwd: root,
            env: &env,
            removed: &[],
            timeout,
        };
        let verdict = run(
            &command,
            &context,
            &root.join("result.log"),
            expected,
            required,
        )
        .unwrap();
        let mut result = serde_json::to_value(verdict).unwrap();
        #[cfg(unix)]
        if let Ok(pid) = fs::read_to_string(root.join("child.pid")) {
            let pid: i32 = pid.trim().parse().unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            let alive = loop {
                let status = Command::new("ps")
                    .args(["-o", "stat=", "-p", &pid.to_string()])
                    .output()
                    .unwrap();
                let status = String::from_utf8_lossy(&status.stdout).trim().to_string();
                let alive = !status.is_empty() && !status.starts_with('Z');
                if !alive || std::time::Instant::now() >= deadline {
                    break alive;
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            if alive {
                // SAFETY: a stray fixture process this test started.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
            result["child_still_running"] = alive.into();
            result["fixture_pid"] = pid.into();
        }
        result
    }

    #[test]
    fn a_custody_mutation_selects_the_extracted_package() {
        let command = test_command("router::properties::epoch", Build::Zmq);
        assert!(command.contains(&"key-custody".to_string()));
        assert!(!command.contains(&"--features".to_string()));
    }

    #[test]
    fn engine_mutations_select_the_requested_features() {
        let command = test_command("work::test", Build::Zmq);
        assert!(command.contains(&"engine".to_string()));
        let at = command.iter().position(|c| c == "--features").unwrap();
        assert_eq!(command[at + 1], "zmq");
    }

    #[test]
    fn every_defect_names_a_distinct_boundary_and_a_real_source_change() {
        let mut names: Vec<&str> = MUTATIONS.iter().map(|m| m.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), MUTATIONS.len());
        for m in MUTATIONS {
            assert!(
                m.before != m.after && m.expected_failure.starts_with("BOUNDARY: "),
                "{}",
                m.name
            );
            let source = fs::read_to_string(crate::root().join(m.path)).unwrap();
            assert_eq!(
                source.matches(m.before).count(),
                m.occurrences,
                "{} no longer matches {}",
                m.name,
                m.path
            );
        }
    }

    #[test]
    fn a_healthy_assertion_is_a_baseline() {
        let r = exercise("#[test] fn healthy(){assert_eq!(1,1);}", 60, None, &[]);
        assert!(r["passed"] == true && r["detected"] == false, "{r}");
    }

    #[test]
    fn an_assertion_failure_detects_a_defect() {
        let r = exercise("#[test] fn defect(){assert_eq!(1,2);}", 60, None, &[]);
        assert!(r["detected"] == true && r["passed"] == false, "{r}");
    }

    #[test]
    fn the_intended_assertion_detects_the_named_defect() {
        let r = exercise(
            r#"#[test] fn defect(){assert_eq!(1,2,"BOUNDARY: money");}"#,
            60,
            Some("BOUNDARY: money"),
            &[],
        );
        assert!(
            r["detected"] == true && r["expected_assertion_seen"] == true,
            "{r}"
        );
    }

    #[test]
    fn a_custom_assertion_message_detects_only_the_intended_boundary() {
        let r = exercise(
            r#"#[test] fn defect(){assert!(false,"assertion failed: BOUNDARY: fifo");}"#,
            60,
            Some("BOUNDARY: fifo"),
            &[],
        );
        assert_eq!(r["detected"], true, "{r}");
    }

    #[test]
    fn the_wrong_assertion_is_invalid() {
        let r = exercise(
            r#"#[test] fn wrong(){assert_eq!(1,2,"BOUNDARY: unrelated");}"#,
            60,
            Some("BOUNDARY: money"),
            &[],
        );
        assert!(
            r["detected"] == false && r["expected_assertion_seen"] == false,
            "{r}"
        );
    }

    #[test]
    fn a_printed_marker_does_not_turn_an_unrelated_assertion_into_detection() {
        let r = exercise(
            r#"#[test] fn wrong(){println!("BOUNDARY: money");assert_eq!(1,2);}"#,
            60,
            Some("BOUNDARY: money"),
            &[],
        );
        assert_eq!(r["detected"], false, "{r}");
    }

    #[test]
    fn required_boundary_counts_must_be_observed() {
        let r = exercise(
            r#"#[test] fn healthy(){println!("ENGINE_BOUNDARY_HITS {{\"money\":2}}");assert_eq!(1,1);}"#,
            60,
            None,
            &["money"],
        );
        assert_eq!(r["passed"], true, "{r}");
        assert_eq!(r["boundary_hits"], json!({"money": 2}));
        let missing = exercise(
            "#[test] fn healthy2(){assert_eq!(1,1);}",
            60,
            None,
            &["money"],
        );
        assert_eq!(missing["passed"], false);
        assert_eq!(missing["missing_boundary_hits"], json!(["money"]));
    }

    #[test]
    fn the_boundary_parser_adds_up_only_positive_integer_counts() {
        let (hits, errors) = boundary_hits("noise\nENGINE_BOUNDARY_HITS {\"money\":2}\nENGINE_BOUNDARY_HITS {\"money\":3,\"retry\":1}\n");
        assert_eq!(
            hits,
            BTreeMap::from([("money".to_string(), 5), ("retry".to_string(), 1)])
        );
        assert_eq!(errors, Vec::<String>::new());
        for malformed in [
            "no JSON",
            "[]",
            "{}",
            "{\"x\":true}",
            "{\"x\":-1}",
            "{\"x\":0}",
            "{\"x\":1.5}",
            "{\"\":1}",
        ] {
            let (hits, errors) = boundary_hits(&format!("ENGINE_BOUNDARY_HITS {malformed}"));
            assert!(hits.is_empty() && !errors.is_empty(), "{malformed}");
        }
    }

    #[test]
    fn a_malformed_boundary_report_invalidates_an_otherwise_healthy_test() {
        let r = exercise(
            r#"#[test] fn healthy3(){println!("ENGINE_BOUNDARY_HITS []");assert_eq!(1,1);}"#,
            60,
            None,
            &[],
        );
        assert_eq!(r["passed"], false, "{r}");
        assert_ne!(r["boundary_hit_errors"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn a_compiler_failure_cannot_count_as_detection() {
        let r = exercise("#[test] fn broken(){unresolved_function();}", 60, None, &[]);
        assert_eq!(r["exit_code"], 101, "{r}");
        assert!(r["ran_one_test"] == false && r["detected"] == false);
    }

    #[test]
    fn zero_tests_cannot_be_a_healthy_baseline() {
        let r = exercise(
            "// A misspelled test filter could select no tests.\n",
            60,
            None,
            &[],
        );
        assert_eq!(r["exit_code"], 0, "{r}");
        assert!(r["passed"] == false && r["detected"] == false);
    }

    #[test]
    fn an_unrelated_runtime_panic_is_invalid() {
        let r = exercise(
            r#"#[test] fn infrastructure(){panic!("worker disconnected");}"#,
            60,
            None,
            &[],
        );
        assert!(r["ran_one_test"] == true && r["detected"] == false, "{r}");
    }

    #[test]
    fn a_hanging_program_is_invalid_and_reaped() {
        let r = exercise(
            r#"#[test] fn hung(){std::fs::write(std::env::var("RUNNER_TEST_PID_PATH").unwrap(), std::process::id().to_string()).unwrap(); std::thread::sleep(std::time::Duration::from_secs(60));}"#,
            20,
            None,
            &[],
        );
        assert!(r["exit_code"].is_null() && r["detected"] == false, "{r}");
        assert_eq!(
            r["ran_one_test"], true,
            "the timeout must come in the test, not the compiler: {r}"
        );
        #[cfg(unix)]
        assert_eq!(
            r["child_still_running"], false,
            "Cargo's test child survived the group timeout: {r}"
        );
    }

    #[test]
    fn rendezvous_and_virtual_deadline_failures_are_invalid() {
        for message in [
            "assertion failed: never reached publication",
            "assertion failed: Elapsed(())",
        ] {
            let r = exercise(
                &format!("#[test] fn infrastructure(){{panic!({message:?});}}"),
                60,
                None,
                &[],
            );
            assert_eq!(r["detected"], false, "{message}: {r}");
        }
    }
}
