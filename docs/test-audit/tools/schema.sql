PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;

-- Functional areas. importance: 1 (nice to have) .. 5 (money, keys, security).
CREATE TABLE IF NOT EXISTS module (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,          -- e.g. "engine/scanner", "monokulo/checkout"
  description TEXT NOT NULL,
  importance INTEGER NOT NULL CHECK (importance BETWEEN 1 AND 5),
  scope TEXT                           -- research scope that defined it (A..E)
);

CREATE TABLE IF NOT EXISTS test (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL,                  -- full test path as the runner reports it
  suite TEXT NOT NULL,                 -- nextest binary id / playwright project+file / node file
  file TEXT,                           -- source file, repo-relative, when known
  duration_ms REAL,                    -- fresh local run on the audited commit
  coverage_lines REAL,                 -- ESTIMATE: lines this test meaningfully exercises (agent judgement)
  coverage_branches REAL,              -- ESTIMATE: branches
  coverage_lines_measured INTEGER,     -- MEASURED: lines covered running this test alone (llvm-cov), NULL if not measured
  coverage_branches_measured INTEGER,  -- MEASURED: branches covered running this test alone
  unique_lines_measured INTEGER,       -- MEASURED: lines no other test in the full run covers (lost if deleted)
  test_type TEXT NOT NULL,             -- unit | integration | e2e | component | presentational | property | contract | tooling
  failed INTEGER NOT NULL DEFAULT 0,   -- 1 if it failed in the fresh local run (noted, not fixed)
  failure_note TEXT,
  value TEXT,                          -- research verdict: keep | merge | delete | rewrite | unsure
  notes TEXT,
  scope TEXT,                          -- research scope that reviewed it (A..E)
  UNIQUE (suite, name)
);

CREATE TABLE IF NOT EXISTS test_modules (
  test_id INTEGER NOT NULL REFERENCES test(id),
  module_id INTEGER NOT NULL REFERENCES module(id),
  PRIMARY KEY (test_id, module_id)
);

CREATE TABLE IF NOT EXISTS test_overlap (
  left_test_id INTEGER NOT NULL REFERENCES test(id),
  right_test_id INTEGER NOT NULL REFERENCES test(id),
  overlap_reason TEXT NOT NULL,
  PRIMARY KEY (left_test_id, right_test_id)
);

CREATE TABLE IF NOT EXISTS suggestion (
  id INTEGER PRIMARY KEY,
  description TEXT NOT NULL,
  estimated_ci_speedup REAL,           -- seconds of serial test time saved per CI run
  estimated_coverage_impact REAL,      -- percentage points of workspace line coverage (negative = lost)
  scope TEXT,
  source TEXT                          -- research-A..E | plan-1..4 | final
);

CREATE TABLE IF NOT EXISTS suggestion_tests (
  suggestion_id INTEGER NOT NULL REFERENCES suggestion(id),
  test_id INTEGER NOT NULL REFERENCES test(id),
  action TEXT NOT NULL,                -- delete | merge-into:<test_id> | rewrite | move-to-e2e | keep
  PRIMARY KEY (suggestion_id, test_id)
);

CREATE VIEW IF NOT EXISTS test_summary AS
SELECT t.id, t.suite, t.name, t.test_type, t.duration_ms, t.failed, t.value,
       group_concat(m.name, ', ') AS modules
FROM test t LEFT JOIN test_modules tm ON tm.test_id = t.id LEFT JOIN module m ON m.id = tm.module_id
GROUP BY t.id;
