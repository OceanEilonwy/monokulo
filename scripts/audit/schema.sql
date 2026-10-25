-- The code audit's working database: what there is to review, who is
-- reviewing it, and what they found. Agents reach it only through
-- scripts/audit/audit.py, which owns every write.

PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS crate (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    path        TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    loc         INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS crate_file (
    id           INTEGER PRIMARY KEY,
    crate_id     INTEGER NOT NULL REFERENCES crate(id),
    path         TEXT NOT NULL UNIQUE,     -- relative to the repository root
    purpose      TEXT NOT NULL DEFAULT '', -- one line, filled in by the reviewer
    loc          INTEGER NOT NULL,
    blob_sha     TEXT NOT NULL,            -- git blob at the audited commit
    is_test      INTEGER NOT NULL DEFAULT 0 CHECK (is_test IN (0, 1)),
    is_tooling   INTEGER NOT NULL DEFAULT 0 CHECK (is_tooling IN (0, 1))
);

-- A feature of the app, cutting across crates.
CREATE TABLE IF NOT EXISTS module (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL,
    focus       TEXT NOT NULL DEFAULT '', -- what a reviewer should look hardest at
    priority    INTEGER NOT NULL DEFAULT 2 CHECK (priority BETWEEN 1 AND 3)
);

-- Every file has one primary module (where it is reviewed) and may be
-- context for others.
CREATE TABLE IF NOT EXISTS module_file (
    module_id     INTEGER NOT NULL REFERENCES module(id),
    crate_file_id INTEGER NOT NULL REFERENCES crate_file(id),
    is_primary    INTEGER NOT NULL DEFAULT 1 CHECK (is_primary IN (0, 1)),
    PRIMARY KEY (module_id, crate_file_id)
);

CREATE VIEW IF NOT EXISTS module_crate AS
    SELECT DISTINCT mf.module_id, cf.crate_id
    FROM module_file mf JOIN crate_file cf ON cf.id = mf.crate_file_id;

-- Hints from tools (clippy with extra lints, cargo-machete). A reviewer
-- triages the ones in their files; they are not findings by themselves.
CREATE TABLE IF NOT EXISTS signal (
    id            INTEGER PRIMARY KEY,
    crate_file_id INTEGER REFERENCES crate_file(id),
    line          INTEGER,
    tool          TEXT NOT NULL,
    code          TEXT NOT NULL,
    message       TEXT NOT NULL
);

-- One pass over a slice of a module (kind = module) or over a theme
-- across the whole workspace (kind = crosscut).
CREATE TABLE IF NOT EXISTS review (
    id         INTEGER PRIMARY KEY,
    module_id  INTEGER REFERENCES module(id),
    kind       TEXT NOT NULL CHECK (kind IN ('module', 'crosscut')),
    title      TEXT NOT NULL,
    brief      TEXT NOT NULL DEFAULT '',
    commit_sha TEXT NOT NULL,
    summary    TEXT NOT NULL DEFAULT ''   -- the reviewer's closing notes
);

CREATE TABLE IF NOT EXISTS review_file (
    review_id     INTEGER NOT NULL REFERENCES review(id),
    crate_file_id INTEGER NOT NULL REFERENCES crate_file(id),
    role          TEXT NOT NULL CHECK (role IN ('target', 'context')),
    PRIMARY KEY (review_id, crate_file_id)
);

CREATE TABLE IF NOT EXISTS review_finding (
    id               INTEGER PRIMARY KEY,
    review_id        INTEGER NOT NULL REFERENCES review(id),
    crate_file_id    INTEGER NOT NULL REFERENCES crate_file(id),
    line_start       INTEGER NOT NULL,
    line_end         INTEGER NOT NULL,
    category         TEXT NOT NULL CHECK (category IN
        ('bug', 'security', 'type_safety', 'simplify', 'dead_code', 'convention')),
    severity         TEXT NOT NULL CHECK (severity IN ('critical', 'high', 'medium', 'low')),
    confidence       TEXT NOT NULL CHECK (confidence IN ('high', 'medium', 'low')),
    title            TEXT NOT NULL,
    problem          TEXT NOT NULL,
    failure_scenario TEXT NOT NULL,   -- concrete input or state, and the wrong outcome
    recommendation   TEXT NOT NULL,
    regression_test  TEXT NOT NULL DEFAULT '',
    reported_by      TEXT NOT NULL,
    status           TEXT NOT NULL DEFAULT 'open' CHECK (status IN
        ('open', 'confirmed', 'rejected', 'duplicate', 'fixed')),
    duplicate_of     INTEGER REFERENCES review_finding(id),
    verified_by      TEXT,
    verify_note      TEXT,
    created_at       TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS job (
    id          INTEGER PRIMARY KEY,
    kind        TEXT NOT NULL CHECK (kind IN ('review', 'verify', 'crosscut')),
    review_id   INTEGER REFERENCES review(id),
    finding_id  INTEGER REFERENCES review_finding(id),
    priority    INTEGER NOT NULL DEFAULT 2,
    status      TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'claimed', 'done')),
    agent       TEXT,
    not_agent   TEXT,              -- a verify job never goes to the finding's reporter
    lease_until TEXT,
    attempts    INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    done_at     TEXT
);

CREATE INDEX IF NOT EXISTS job_queue ON job (status, kind, priority);
CREATE INDEX IF NOT EXISTS finding_file ON review_finding (crate_file_id);
CREATE INDEX IF NOT EXISTS signal_file ON signal (crate_file_id);

-- Findings with their file path and module, for querying by hand.
CREATE VIEW IF NOT EXISTS finding_view AS
    SELECT rf.*, cf.path, coalesce(m.name, 'crosscut: ' || r.title) AS area
    FROM review_finding rf
    JOIN crate_file cf ON cf.id = rf.crate_file_id
    JOIN review r ON r.id = rf.review_id
    LEFT JOIN module m ON m.id = r.module_id;

-- How each confirmed finding should be fixed and what (if anything) should
-- test it. A test has to earn its place: a fix that makes the bug
-- impossible by construction needs none, unless the property is critical.
CREATE TABLE IF NOT EXISTS fix_plan (
    finding_id INTEGER PRIMARY KEY REFERENCES review_finding(id),
    fix_group  TEXT NOT NULL,   -- findings fixed together share a group
    approach   TEXT NOT NULL CHECK (approach IN
        ('construction', 'construction+test', 'extend_test', 'new_test', 'no_test')),
    fix        TEXT NOT NULL,   -- the fix, concretely
    test_where TEXT NOT NULL DEFAULT '',  -- existing test to extend, or where a new one goes
    test_what  TEXT NOT NULL DEFAULT '',  -- the property it asserts
    rationale  TEXT NOT NULL,   -- why this approach, and why a test earns (or doesn't earn) its place
    effort     TEXT NOT NULL CHECK (effort IN ('S', 'M', 'L')),
    planned_by TEXT NOT NULL
);
