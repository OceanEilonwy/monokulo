# Plan review brief (shared by the 4 plan reviewers)

You are reviewing a test audit of the Monokulo repo and writing an action plan. The user reviews the
final plan in the morning. Nothing gets changed in code now: **no PRs, no commits, no edits outside
`target/test-audit/`**.

## Read first (all under /home/henry/Downloads/mokulo/.claude/worktrees/test-audit/target/test-audit/)
- BRIEF.md: the user's goal and what counts as cruft (read the "goal" section closely)
- RESEARCH.md: the 5 research scopes' findings
- TIMING_FACTS.md: local and CI timings
- DECISIONS.md: calls already made
- audit.sqlite: the evidence (tables module, test, test_modules, test_overlap, suggestion,
  suggestion_tests; view test_summary). Query it with
  `sqlite3 -cmd '.timeout 30000' audit.sqlite`.
- The code itself, in the worktree (audited commit f7fb5c9be).
- User preferences: /home/henry/.claude/projects/-home-henry-Downloads-mokulo/memory/ (MEMORY.md is the
  index; feedback_tests_run_production_values.md, feedback_tests_in_process_http_named.md,
  feedback_remove_dead_code_including_tests.md, project_test_time_targets.md are the most relevant)

## The two goals your plan must serve
1. **Cut the cruft.** No presentational "my code matches my code" tests. UI tested only as functional
   e2e flows (edit page settings, register, login, create wallet, create order, POS new/background/
   resume, plus any genuine extra journeys). Keep the tests that guard money, keys, security and
   real invariants.
2. **Time targets** (set by the user, both must be addressed with options backed by facts):
   - North star: the **whole local test suite, excluding compilation, in under 1 minute**.
   - Hard limit: **no CI test segment longer than 5 minutes** (read as each CI step that runs
     tests, but also say what it means if read as a job including its build).
   Property, fuzz, scale and WooCommerce tests were excluded from the cruft audit at the user's
   request. For the time targets you may still propose options that touch them (e.g. moving the
   212 property tests, 645 s serial, out of the default run into the existing scheduled
   engine-properties workflow). Mark any such option clearly as touching an excluded suite.

## Do not take research verdicts on trust
Spot-check them. Pick at least 15 tests marked delete or merge (weighted toward high-importance
modules) and read the code: is the verdict right? Report what you checked and any verdict you'd
overturn. Equally, flag anything marked keep that is plainly cruft.

## Output
Write your plan to `plans/plan-<your number>.md` (create the folder if needed). Keep it under about
1500 words, structured as:
1. Summary: what you'd do, and the expected result in tests removed, serial seconds saved, local wall
   time, longest CI test step, and coverage pp lost.
2. Cruft-cut actions, as numbered actions with the evidence (suggestion ids, test ids or counts,
   measured numbers).
3. Time-target options: for each, the facts, the expected effect on the local wall time and on the
   longest CI step, the cost and the risk. Then say which combination reaches each target, or why
   one can't be reached and how close it gets.
4. Verdicts you overturned, from your spot-checks.
5. Sequencing: the order of work, with an effort estimate per step.
6. Open questions only the user can answer.

Also insert your plan's actions into the DB: `suggestion` rows with `source = 'plan-<n>'`, plus
`suggestion_tests` rows when an action names specific tests. You may reuse test ids from the research
suggestions.

Reply with a short summary (under 300 words) and the path to your plan file.
