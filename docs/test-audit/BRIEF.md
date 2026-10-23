# Test audit: research brief (shared by every research agent)

## The goal (the user's words, condensed)

The test count is out of hand. The user wants to **seriously cut the cruft**. The research builds an
evidence base. It changes no code.

What counts as cruft:
- Tests that assert obvious presentational output: "when I load the page for order X, the HTML shows X",
  a label renders, a CSS class is present, markup matches a string. These only check that the code
  matches the code. Low value.
- Tests that duplicate another test's behaviour, or test a trivial getter, constructor, Display impl or
  serde round trip of a plain struct.
- UI tests that check UI detail "to the n-th degree".

What the user wants kept for the UI: **functional end-to-end flows** that show the system can be used:
1. I can edit page settings
2. I can register
3. I can log in
4. I can create a new wallet
5. I can create an order
6. I can use the POS (new order, send to background, bring back up)

You may propose a few more flows of the same kind if you find a functional journey that matters (for
example "a customer pays an order and it confirms", "a plugin connects"). Name each one as a flow, not
as a UI detail.

Non-UI tests (engine logic, payment detection, scanning, key custody, crypto, migrations, auth and
security boundaries, rate limits, webhooks signing, money arithmetic) get judged on value. Do not delete
them wholesale. A test that pins real behaviour that could regress silently and cost money, keys or
security is high value even if it is slow. Flag slow ones that could be made cheaper.

**Excluded from the audit** (leave alone, make no suggestions about them): property tests (rows with
`test_type = 'property'`, names containing `::properties::` or `_properties::`, proptest files), fuzz,
scale, the WooCommerce PHP plugin, live-network (`live_`, ignored) tests.

## Where things are

- Worktree (audited commit, origin/main): `/home/henry/Downloads/mokulo/.claude/worktrees/test-audit`.
  Read code there. Do not edit any file under it except in `target/test-audit/`.
- Audit DB: `target/test-audit/audit.sqlite` (schema in `target/test-audit/tools/schema.sql`). The test
  table is already loaded from a fresh local run: name, suite, duration_ms, failed, failure_note.
  Rust `suite` is the nextest binary id (e.g. `monokulo`, `engine::verification`); e2e `suite` is the
  Playwright project; `file` holds the spec file for e2e.
- Fresh coverage report: `target/coverage/rust/` (llvm-cov HTML, `raw.json` with per-file totals) and
  `target/coverage/browser/`. Baseline lcov of the full Rust run: `target/test-audit/baseline.json`.
- Project memory with the user's preferences (read the relevant ones):
  `/home/henry/.claude/projects/-home-henry-Downloads-mokulo/memory/` (MEMORY.md is the index).
- Use `cargo nextest`, never plain `cargo test`.

## What to write into the DB (use `sqlite3 -cmd '.timeout 30000' target/test-audit/audit.sqlite`)

Other agents write at the same time; keep transactions short and always set the timeout.

1. `module` rows for the functional areas in your scope. Name them `<crate-or-app>/<area>`, for example
   `engine/scanner`, `monokulo/checkout`, `e2e/pos`. Description: one sentence. Importance 1..5
   (5 = money, keys or security; 1 = cosmetic). Set `scope` to your letter. Reuse a module another
   scope already made rather than creating a near-duplicate (check first).
2. For **every** non-excluded test in your scope, update its row:
   - `file` (repo-relative source file) if empty
   - `test_type`: unit | integration | e2e | component | presentational | contract | tooling.
     Use `presentational` for tests whose main assertion is that rendered output contains something.
   - `coverage_lines`, `coverage_branches`: your ESTIMATE of how many lines and branches of production
     code the test meaningfully exercises (order of magnitude is fine).
   - `value`: keep | merge | delete | rewrite | unsure. `notes`: one short reason. `scope`: your letter.
   - `test_modules` links (one or more modules).
3. `test_overlap` rows for pairs that test the same behaviour (put the lower id on the left). Reason in
   one sentence. Do not list every pair in a big family: link each duplicate to the one test you'd keep.
4. `suggestion` rows (with `source = 'research-<letter>'`, `scope = '<letter>'`) and `suggestion_tests`
   rows: group the work into concrete actions ("delete the 40 views::* tests that assert the order id is
   in the HTML", "merge the 12 rate-limit tests into one table test"). Give `estimated_ci_speedup` in
   seconds of serial test time (sum of the durations removed, minus what a merged replacement costs) and
   `estimated_coverage_impact` in percentage points of workspace line coverage (negative = lost; use
   measured numbers when you have them). `action`: delete | merge-into:<test_id> | rewrite | move-to-e2e
   | keep.

## Measured coverage (Rust only)

Where it helps decide, measure instead of guessing. The helper runs one coverage measurement at a time
(calls queue behind each other, each takes several minutes), so measure **groups**, not every test:

    target/test-audit/tools/measure.sh without '<nextest filterset>'   # lines the suite loses without them
    target/test-audit/tools/measure.sh alone   '<nextest filterset>'   # lines they cover on their own

Filtersets are nextest's, e.g. `binary_id(monokulo) & test(/^views::order::/)` or
`test(=http::tests::rate_limit_returns_429)`. Run `without` on your main deletion groups: a group whose
removal loses ~0 lines is safe to cut. When you measure, write the result into the tests:
`coverage_lines_measured`/`coverage_branches_measured` from `alone` on a single test, and
`unique_lines_measured` (lines lost) from `without` on a single test. For a group, put the group's
numbers in the suggestion's `estimated_coverage_impact` and mention them in the description.
Budget: at most 3 measurement calls per scope (they queue across 5 agents; start them early, in the background, and keep reviewing while they run).

## Failures

Some tests failed in the fresh run (`failed = 1`). Do not fix them. If a failing test is in your scope,
say in `notes` whether it looks like a real bug, a flaky test, or an environment issue.

## Your report back (keep it under 600 words)

- Counts: tests reviewed, by verdict, by test_type.
- Your modules with importance.
- Your top suggestions with speedup and coverage impact.
- Measurements you ran and what they showed.
- Anything surprising (dead code, tests that test nothing, gaps in the 6 flows, failures).
