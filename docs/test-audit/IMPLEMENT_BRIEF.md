# Implementing the test audit: shared brief for all four agents

You are one of four agents implementing an agreed test-suite plan in the mokulo repo, in parallel, each in its own git worktree and branch. The repo owner is away and has authorised the work. Make sound decisions yourself, and log every decision.

## Read first (absolute paths, read-only; do not copy these docs into your branch)
- Plan: `/home/henry/Downloads/mokulo/.claude/worktrees/test-audit/docs/test-audit-plan.md` (sections 2, 3, 4 and 6, plus the "Your answers" paragraph at the end of section 5)
- Decisions: `/home/henry/Downloads/mokulo/.claude/worktrees/test-audit/docs/test-audit/DECISIONS.md` (items 18-29 are the owner's answers; they win over anything older)
- Reviewer plans with the per-test reasoning: `/home/henry/Downloads/mokulo/.claude/worktrees/test-audit/docs/test-audit/plan-1.md` .. `plan-4.md`, `RESEARCH.md`, `TIMING_FACTS.md`
- DB (SQLite, read-only): `/home/henry/Downloads/mokulo/.claude/worktrees/test-audit/target/test-audit/audit.sqlite`. Open with `sqlite3 -readonly`.
  - Your work list: `SELECT s.description, t.id, t.name, t.file, t.suite, st.action, t.notes FROM suggestion s JOIN suggestion_tests st ON st.suggestion_id=s.id JOIN test t ON t.id=st.test_id WHERE s.source='final' AND <your filter>`.
  - `action` is `delete`, `merge-into:<test_id>` (fold into that test as a table row, or as an extra step of that flow), `rewrite` (`t.notes` and the plans say how) or `keep`.
  - Rows with `source='final'` and no linked tests (N1-N4, T-a..T-k) are described in the plan.
  - The DB was built on commit f7fb5c9be; origin/main has since moved about 41 commits. Find each test by name and file in the current code. If a test was renamed, moved or already deleted upstream, note it and move on.

## Rules that are not negotiable (the owner's standing rules)
- **Tests use production values.** No `cfg(test)` value swaps. To make something faster, pass an explicit, validated tuning: a struct with `const DEFAULT`, a `validate()`, and tests calling `with_tuning(..)` explicitly, as `work::ScanTuning` in `crates/engine/src/work/tuning.rs` does, or `StreamTuning` in `crates/engine/src/activity.rs`.
- **Settings** come from the TOML options file plus the CLI and go through the settings registry. An invalid value stops the process. Runtime switches live in the DB, and secrets come only from the environment.
- **Run Rust tests with `cargo nextest run`, never `cargo test`.** Plain cargo test shares a process-wide log throttle and fails 4 engine tests. Doctests are the exception: `cargo test --doc`.
- **Dead code:** code that only tests call is dead. Remove it together with its tests, and port any test that still matters onto live code.
- **Tests reach the engine in-process.** The few that use real HTTP say `over_http` in their name.
- **Before deleting a test, grep the repo for its name.** Names are listed in `.config/nextest.toml` (priorities and groups), the `NOT_DAILY` list in live.rs, Playwright `grepInvert`/`testMatch`, mutation-catalogue `before` strings, the coverage-gallery `required` list and doc comments. Update every place that names it.
- **Gallery feeders:** keep the single feeder of each required gallery group (2684, 2692, 2658, and 2609/2615), and check `e2e/browser/coverage-gallery-reporter.js`'s `required` list.
- **No overreach:** don't add features, don't reformat unrelated code, and match the surrounding code's comment density and naming. Commit messages are plain English sentences like the repo's history (`git log --oneline -20`), ending with the line `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Isolation: what keeps the four of you from interfering
- **Work only in your own worktree.** Before your first edit: `git fetch origin && git reset --hard origin/main`. Your worktree is fresh, so this only moves it to the newest main.
- **Edit only the files your chunk owns** (listed in your prompt). If a fix needs a file another chunk owns, don't edit it. Write the need into your progress file under "Needs from other chunks"; the orchestrator integrates it.
- **Run only your own tests**, scoped exactly as your prompt says. Never run the whole workspace suite, `cargo xtask coverage`, or another chunk's runner. The orchestrator runs the combined suite after integrating. Use `nice -n 10` for cargo builds and tests, so four agents share the 32 cores politely.
- **Ports and temp files:** use your worktree's `target/` and a temp directory under it, never a shared `/tmp` path.

## How to work
- **Small commits, one per logical slice**, for example one per module group or spec family. Run your scoped tests before each commit. A commit must leave your scoped tests green, except for tests that were already failing on origin/main: record those, don't fix them.
- **Push your branch after each commit** with `git push -u origin HEAD`. Never push to main, never force-push, never merge, and don't open PRs.
- **Keep a progress log** at `docs/test-audit/progress/<chunk>.md` in your branch, and commit it. It holds:
  - what's done (with test counts removed or merged or added)
  - the decisions you made and why
  - deviations from the plan (for example a test the plan said to delete that turned out to be the only pin on a money, key or security rule: keep it and say so)
  - tests already failing on main
  - "Needs from other chunks"
  - before/after timings for your scope
- **Before you delete anything**, read the test and the test the DB says covers it instead. The plans' spot-checks overturned 14 verdicts. If you find another one wrong, especially anything touching auth, money, keys, custody or privacy, keep the test and log it.
- **If you hit a usage or rate limit, or an interruption,** carry on from your progress log when you resume.
- **When you finish**, reply with a short report: your branch name, the last commit, counts, your scoped test results with timings, deviations, and "Needs from other chunks".
